// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Preparing the hosts that run sandboxes: pulling the default sandbox image
//! before the first create needs it.
//!
//! The first sandbox on a host used to pull its image (gigabytes) inside the
//! create. Nothing here happens on its own: an operator asks to prepare the
//! hosts eligible for sandbox placement (or one of them), which turns on
//! `keep_prepared` for each, audit-logged, and starts the pull. From then on
//! Temps keeps those hosts prepared: when an upgrade changes the default
//! image it pulls the new one and removes the one it prepared before, and an
//! operator can turn that off per host.
//!
//! What is pulled is exactly what a create uses: the hosts' providers resolve
//! the default image themselves (the release-pinned reference on the control
//! plane; the operator's configured image, or the node's own pinned default,
//! on a worker), and the pull runs in the host's shared pull tracker, so a
//! create that starts meanwhile waits on the same pull rather than starting
//! a second one.
//!
//! Background work is bounded: one watcher per host while its pull runs
//! (polling the host's status every [`WATCH_INTERVAL`] until it ends, at most
//! [`WATCH_DEADLINE`]), and a reconciler that wakes every
//! [`RECONCILE_INTERVAL`] and looks only at hosts with `keep_prepared` on,
//! one status call each; it acts only on a change (a new default, a finished
//! pull, an image left to remove).

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::{stream, StreamExt};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection, DbErr,
    EntityTrait, IntoActiveModel, QueryFilter, Set, Statement, TransactionTrait,
};
use serde::Serialize;
use utoipa::ToSchema;

use temps_entities::sandbox_image_preparations;
use temps_sandbox_providers::{
    ImagePrepareState, ImagePrepareStatus, ImageRemoval, SandboxProvider,
};

use crate::error::SandboxError;
use crate::services::placement::{PlacementNode, CONTROL_PLANE_NAME};
use crate::services::SandboxService;

/// How often a running pull's host is asked for its status.
pub const WATCH_INTERVAL: Duration = Duration::from_secs(2);
/// Longest a watcher follows one pull: the provider's own ceiling on a pull
/// plus slack. A pull still running then is picked up by the reconciler.
pub const WATCH_DEADLINE: Duration = Duration::from_secs(3 * 60 * 60 + 10 * 60);
/// How often hosts with `keep_prepared` are checked for a changed default.
pub const RECONCILE_INTERVAL: Duration = Duration::from_secs(30 * 60);
/// Delay before the first reconcile after startup, so it does not compete
/// with the server starting up.
pub const RECONCILE_STARTUP_DELAY: Duration = Duration::from_secs(2 * 60);
/// A failed preparation is retried by the reconciler after this long; an
/// operator's explicit "prepare" retries at once.
pub const RETRY_FAILED_AFTER: chrono::Duration = chrono::Duration::hours(1);
/// Deadline for one status or prepare call to a host.
const HOST_CALL_DEADLINE: Duration = Duration::from_secs(10);
/// Hosts asked at once when listing statuses.
const STATUS_CONCURRENCY: usize = 8;

/// Why a sandbox image preparation request failed.
#[derive(Debug, thiserror::Error)]
pub enum ImagePreparationError {
    #[error(
        "Database error while {operation} the sandbox image preparation of host '{host}': {source}"
    )]
    Database {
        operation: &'static str,
        host: String,
        #[source]
        source: DbErr,
    },
    #[error("No sandbox host '{host}' exists (use a node id, or 0 for the control plane)")]
    HostNotFound { host: String },
    #[error("Host '{host}' cannot take sandboxes, so its sandbox image is not prepared: {reason}")]
    HostNotEligible { host: String, reason: String },
    #[error("No host is eligible for sandbox placement, so there is nothing to prepare; allow a node in the sandbox placement settings first")]
    NoEligibleHost,
    #[error("Listing the hosts that run sandboxes failed: {source}")]
    Placement {
        #[source]
        source: Box<SandboxError>,
    },
}

/// One host's sandbox image preparation, as the API reports it.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HostImagePreparation {
    /// Node id; `0` for the control plane.
    pub node_id: i32,
    pub node_name: String,
    pub is_control_plane: bool,
    /// The host can take new sandboxes, so it is worth preparing.
    pub eligible: bool,
    /// Why the host is not eligible, when it is not.
    pub ineligible_reason: Option<String>,
    /// Temps keeps this host's default sandbox image pulled, re-pulling it
    /// when the default changes. Turned on by "prepare".
    pub keep_prepared: bool,
    /// Where the host stands with its default image right now. `None` when
    /// the host was not asked (not eligible) or could not be reached, with
    /// the reason in `status_error`.
    pub status: Option<ImagePrepareStatus>,
    /// Why the status could not be read.
    pub status_error: Option<String>,
    /// The image Temps last prepared on this host.
    pub prepared_image: Option<String>,
    /// Previously prepared images still to remove (a container used them).
    pub stale_images: Vec<String>,
    /// Why the last preparation failed.
    pub last_error: Option<String>,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_error_at: Option<DateTime<Utc>>,
}

/// Every host that can run sandboxes and its image preparation.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SandboxImagePreparationResponse {
    /// Control plane first, then workers by name.
    pub hosts: Vec<HostImagePreparation>,
    /// Every eligible host has its default image: the next create on any of
    /// them starts without a download.
    pub all_ready: bool,
}

/// What the reconciler or a watcher does for one host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HostAction {
    Nothing,
    /// Start pulling the host's current default image.
    Prepare,
    /// A pull of `image` is running: follow it.
    Watch {
        image: String,
    },
    /// `image` is on the host and is not yet recorded as prepared: record
    /// it, and retire the image prepared before.
    Record {
        image: String,
    },
}

/// Decide what keeping a host prepared needs, from its row and its live
/// status. A changed default shows up as the new image not being present
/// (`NotPulled`), or present after a create pulled it (`Ready` with a
/// different image than the one recorded).
pub(crate) fn plan_host(
    row: &sandbox_image_preparations::Model,
    status: &ImagePrepareStatus,
    now: DateTime<Utc>,
) -> HostAction {
    if !row.keep_prepared {
        return HostAction::Nothing;
    }
    match status.state {
        ImagePrepareState::Ready => {
            if row.prepared_image.as_deref() == Some(status.image.as_str()) {
                HostAction::Nothing
            } else {
                HostAction::Record {
                    image: status.image.clone(),
                }
            }
        }
        ImagePrepareState::Pulling => HostAction::Watch {
            image: status.image.clone(),
        },
        ImagePrepareState::NotPulled => HostAction::Prepare,
        ImagePrepareState::Failed => {
            if row
                .last_error_at
                .is_none_or(|at| now - at >= RETRY_FAILED_AFTER)
            {
                HostAction::Prepare
            } else {
                HostAction::Nothing
            }
        }
        ImagePrepareState::Unsupported => HostAction::Nothing,
    }
}

/// Images to remove once `ready` is the host's prepared image: the one
/// prepared before it and every one still pending removal, never `ready`
/// itself.
pub(crate) fn images_to_retire(
    row: &sandbox_image_preparations::Model,
    ready: &str,
) -> Vec<String> {
    let mut retire = Vec::new();
    for image in row
        .prepared_image
        .as_deref()
        .into_iter()
        .chain(row.stale_images.iter().map(String::as_str))
    {
        if image != ready && !retire.iter().any(|r: &String| r == image) {
            retire.push(image.to_string());
        }
    }
    retire
}

/// What a newly ready `ready` image does to the host's older images:
/// `(remove now, keep pending removal)`. A host kept prepared tries to
/// remove all of them. A host whose policy was turned off removes nothing,
/// but every older image (the one prepared before included) stays recorded,
/// so turning the policy back on can still clean them up.
pub(crate) fn outgoing_images(
    row: &sandbox_image_preparations::Model,
    ready: &str,
) -> (Vec<String>, Vec<String>) {
    let outgoing = images_to_retire(row, ready);
    if row.keep_prepared {
        (outgoing, Vec::new())
    } else {
        (Vec::new(), outgoing)
    }
}

/// Take `removed` out of row `id`'s `stale_images` as it is stored now,
/// keeping the rest in order. A removal pass reads the row, then waits on
/// the host for each image; a pull finishing meanwhile can add images to
/// the list. Writing back the list read before the wait would drop those,
/// and an image no list records is never cleaned up.
pub(crate) fn subtract_removed_statement(id: i32, removed: Vec<String>) -> Statement {
    Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE sandbox_image_preparations SET stale_images = ARRAY( \
             SELECT entry.image FROM unnest(stale_images) WITH ORDINALITY AS entry(image, position) \
             WHERE NOT (entry.image = ANY($2)) ORDER BY entry.position \
         ), updated_at = now() WHERE id = $1",
        [id.into(), removed.into()],
    )
}

/// Upsert a host's row with `keep_prepared = keep`.
async fn upsert_keep<C: ConnectionTrait>(
    conn: &C,
    host: &str,
    api_id: i32,
    keep: bool,
) -> Result<sandbox_image_preparations::Model, ImagePreparationError> {
    let statement = Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO sandbox_image_preparations (node_id, keep_prepared) VALUES ($1, $2) \
         ON CONFLICT ((COALESCE(node_id, 0))) \
         DO UPDATE SET keep_prepared = EXCLUDED.keep_prepared, updated_at = now() \
         RETURNING *",
        [row_node_id(api_id).into(), keep.into()],
    );
    sandbox_image_preparations::Entity::find()
        .from_raw_sql(statement)
        .one(conn)
        .await
        .and_then(|row| {
            row.ok_or_else(|| {
                DbErr::RecordNotFound(format!(
                    "sandbox_image_preparations row for node {api_id} after upsert"
                ))
            })
        })
        .map_err(|source| ImagePreparationError::Database {
            operation: "saving",
            host: host.to_string(),
            source,
        })
}

/// Most images kept awaiting removal per host (the column's CHECK).
pub(crate) const MAX_STALE_IMAGES: usize = 16;

/// The images still awaiting removal, newest first, within
/// [`MAX_STALE_IMAGES`]. Past the cap the oldest are forgotten (logged):
/// they stay on the host as ordinary cached images.
pub(crate) fn pending_removal(host: &str, mut still_used: Vec<String>) -> Vec<String> {
    if still_used.len() > MAX_STALE_IMAGES {
        for forgotten in still_used.drain(MAX_STALE_IMAGES..) {
            tracing::warn!(
                host = %host,
                image = %forgotten,
                "too many previous sandbox images still in use; no longer tracking this one for removal"
            );
        }
    }
    still_used
}

/// The API's node id for a host: `0` for the control plane.
fn api_node_id(node_id: Option<i32>) -> i32 {
    node_id.unwrap_or(0)
}

fn row_node_id(api_id: i32) -> Option<i32> {
    (api_id != 0).then_some(api_id)
}

pub struct ImagePreparationService {
    db: Arc<DatabaseConnection>,
    provider: Arc<dyn SandboxProvider>,
    sandbox_service: Arc<SandboxService>,
    /// Hosts (API node id) with a watcher following their pull.
    watching: Mutex<HashSet<i32>>,
}

impl ImagePreparationService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        provider: Arc<dyn SandboxProvider>,
        sandbox_service: Arc<SandboxService>,
    ) -> Self {
        Self {
            db,
            provider,
            sandbox_service,
            watching: Mutex::new(HashSet::new()),
        }
    }

    async fn hosts(&self) -> Result<Vec<PlacementNode>, ImagePreparationError> {
        self.sandbox_service
            .placement_nodes()
            .await
            .map_err(|source| ImagePreparationError::Placement {
                source: Box::new(source),
            })
    }

    async fn rows(&self) -> Result<Vec<sandbox_image_preparations::Model>, ImagePreparationError> {
        sandbox_image_preparations::Entity::find()
            .all(self.db.as_ref())
            .await
            .map_err(|source| ImagePreparationError::Database {
                operation: "reading",
                host: "every host".to_string(),
                source,
            })
    }

    /// Every host that runs sandboxes, with its live image status.
    pub async fn status(&self) -> Result<SandboxImagePreparationResponse, ImagePreparationError> {
        let hosts = self.hosts().await?;
        let rows = self.rows().await?;
        let provider = Arc::clone(&self.provider);
        let mut listed: Vec<HostImagePreparation> = stream::iter(hosts)
            .map(|host| {
                let provider = Arc::clone(&provider);
                let row = rows
                    .iter()
                    .find(|r| api_node_id(r.node_id) == host.id)
                    .cloned();
                async move { describe_host(provider.as_ref(), host, row).await }
            })
            .buffered(STATUS_CONCURRENCY)
            .collect()
            .await;
        listed.sort_by(|a, b| {
            b.is_control_plane
                .cmp(&a.is_control_plane)
                .then_with(|| a.node_name.cmp(&b.node_name))
        });
        let all_ready = listed.iter().any(|h| h.eligible)
            && listed.iter().filter(|h| h.eligible).all(|h| {
                h.status
                    .as_ref()
                    .is_some_and(|s| s.state == ImagePrepareState::Ready)
            });
        Ok(SandboxImagePreparationResponse {
            hosts: listed,
            all_ready,
        })
    }

    /// Prepare `node` (API id; `None` = every eligible host): turn
    /// `keep_prepared` on and start pulling its default image. Returns the
    /// hosts it was asked of.
    pub async fn prepare(
        self: &Arc<Self>,
        node: Option<i32>,
    ) -> Result<Vec<i32>, ImagePreparationError> {
        let hosts = self.hosts().await?;
        let targets: Vec<PlacementNode> = match node {
            Some(id) => {
                let host = hosts.into_iter().find(|h| h.id == id).ok_or_else(|| {
                    ImagePreparationError::HostNotFound {
                        host: id.to_string(),
                    }
                })?;
                if !host.eligible {
                    return Err(ImagePreparationError::HostNotEligible {
                        reason: host.reason.clone().unwrap_or_else(|| host.status.clone()),
                        host: host.name,
                    });
                }
                vec![host]
            }
            None => {
                let eligible: Vec<PlacementNode> =
                    hosts.into_iter().filter(|h| h.eligible).collect();
                if eligible.is_empty() {
                    return Err(ImagePreparationError::NoEligibleHost);
                }
                eligible
            }
        };
        // Every host's policy in one transaction, so the caller audits
        // exactly what was saved: all of it, or (on an error) nothing.
        let txn = self
            .db
            .begin()
            .await
            .map_err(|source| ImagePreparationError::Database {
                operation: "starting to save",
                host: "the hosts to prepare".to_string(),
                source,
            })?;
        let mut rows = Vec::with_capacity(targets.len());
        for host in &targets {
            let row = upsert_keep(&txn, &host.name, host.id, true).await?;
            rows.push((host.name.clone(), row));
        }
        txn.commit()
            .await
            .map_err(|source| ImagePreparationError::Database {
                operation: "saving",
                host: "the hosts to prepare".to_string(),
                source,
            })?;
        let prepared = targets.iter().map(|h| h.id).collect();

        // Asking a host to pull can take up to HOST_CALL_DEADLINE when it is
        // unresponsive: done in the background, a few hosts at a time, so
        // the request returns at once. Progress is read from `status`.
        let service = Arc::clone(self);
        tokio::spawn(async move {
            stream::iter(rows)
                .for_each_concurrent(STATUS_CONCURRENCY, |(host, row)| {
                    let service = Arc::clone(&service);
                    async move { service.start_host(&host, row).await }
                })
                .await;
        });
        Ok(prepared)
    }

    /// Turn keeping `node` prepared on or off. Turning it off pulls and
    /// removes nothing; the image already pulled stays cached.
    pub async fn set_keep_prepared(
        &self,
        node: i32,
        keep: bool,
    ) -> Result<sandbox_image_preparations::Model, ImagePreparationError> {
        let hosts = self.hosts().await?;
        let host = hosts.into_iter().find(|h| h.id == node).ok_or_else(|| {
            ImagePreparationError::HostNotFound {
                host: node.to_string(),
            }
        })?;
        upsert_keep(self.db.as_ref(), &host.name, host.id, keep).await
    }

    /// Ask the host to pull its default image and follow the pull.
    async fn start_host(self: &Arc<Self>, host: &str, row: sandbox_image_preparations::Model) {
        let node_id = row.node_id;
        let started = tokio::time::timeout(
            HOST_CALL_DEADLINE,
            self.provider.prepare_image_on(node_id, None, None),
        )
        .await;
        let status = match started {
            Ok(Ok(status)) => status,
            Ok(Err(error)) => {
                self.record_failure(host, &row, &error.to_string()).await;
                return;
            }
            Err(_) => {
                self.record_failure(
                    host,
                    &row,
                    &format!(
                        "host '{host}' did not answer the prepare request within {}s",
                        HOST_CALL_DEADLINE.as_secs()
                    ),
                )
                .await;
                return;
            }
        };
        tracing::info!(
            host = %host,
            image = %status.image,
            state = ?status.state,
            "sandbox image preparation requested"
        );
        self.follow(host.to_string(), row, status).await;
    }

    /// Act on a status a host just reported: record a finished pull, or
    /// start a watcher for a running one.
    async fn follow(
        self: &Arc<Self>,
        host: String,
        row: sandbox_image_preparations::Model,
        status: ImagePrepareStatus,
    ) {
        match status.state {
            ImagePrepareState::Ready => self.record_ready(&host, &row, &status.image).await,
            ImagePrepareState::Failed => {
                let reason = status
                    .error
                    .clone()
                    .unwrap_or_else(|| format!("pulling {} failed", status.image));
                self.record_failure(&host, &row, &reason).await;
            }
            ImagePrepareState::Pulling => self.spawn_watcher(host, row, status.image),
            ImagePrepareState::NotPulled | ImagePrepareState::Unsupported => {}
        }
    }

    /// Follow the pull of `image` on a host until it ends. At most one
    /// watcher per host.
    fn spawn_watcher(
        self: &Arc<Self>,
        host: String,
        row: sandbox_image_preparations::Model,
        image: String,
    ) {
        let key = api_node_id(row.node_id);
        {
            let mut watching = self.watching.lock().unwrap_or_else(PoisonError::into_inner);
            if !watching.insert(key) {
                return;
            }
        }
        let service = Arc::clone(self);
        tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + WATCH_DEADLINE;
            let outcome = loop {
                tokio::time::sleep(WATCH_INTERVAL).await;
                if tokio::time::Instant::now() >= deadline {
                    break None;
                }
                let status = tokio::time::timeout(
                    HOST_CALL_DEADLINE,
                    service
                        .provider
                        .image_prepare_status_on(row.node_id, Some(&image), None),
                )
                .await;
                match status {
                    Ok(Ok(status)) if status.state.is_terminal() => break Some(status),
                    Ok(Ok(_)) => {}
                    // A host that cannot answer for a moment is asked again;
                    // the deadline bounds how long.
                    Ok(Err(error)) => {
                        tracing::debug!(host = %host, image = %image, %error, "sandbox image status unavailable")
                    }
                    Err(_) => {
                        tracing::debug!(host = %host, image = %image, "sandbox image status timed out")
                    }
                }
            };
            match outcome {
                Some(status) => {
                    // Re-read: the operator may have changed the row since.
                    let row = service.reload(&row).await.unwrap_or(row);
                    service.follow_terminal(&host, &row, status).await;
                }
                None => tracing::warn!(
                    host = %host,
                    image = %image,
                    "stopped following a sandbox image pull that outlived its deadline; \
                     the reconciler checks the host again later"
                ),
            }
            service
                .watching
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&key);
        });
    }

    async fn follow_terminal(
        &self,
        host: &str,
        row: &sandbox_image_preparations::Model,
        status: ImagePrepareStatus,
    ) {
        match status.state {
            ImagePrepareState::Ready => self.record_ready(host, row, &status.image).await,
            ImagePrepareState::Failed => {
                let reason = status
                    .error
                    .unwrap_or_else(|| format!("pulling {} failed", status.image));
                self.record_failure(host, row, &reason).await;
            }
            ImagePrepareState::NotPulled
            | ImagePrepareState::Pulling
            | ImagePrepareState::Unsupported => {}
        }
    }

    async fn reload(
        &self,
        row: &sandbox_image_preparations::Model,
    ) -> Option<sandbox_image_preparations::Model> {
        sandbox_image_preparations::Entity::find_by_id(row.id)
            .one(self.db.as_ref())
            .await
            .ok()
            .flatten()
    }

    /// `image` is ready on the host: record it, then retire the images
    /// prepared before it. An image a container still uses stays, recorded
    /// in `stale_images` for a later attempt.
    ///
    /// A host whose operator turned `keep_prepared` off while the pull ran
    /// gets the result recorded and nothing removed: retiring images is
    /// part of keeping a host prepared, and that is no longer asked for.
    /// The older images stay in `stale_images` (see [`outgoing_images`]).
    async fn record_ready(&self, host: &str, row: &sandbox_image_preparations::Model, image: &str) {
        let (retire, mut still_used) = outgoing_images(row, image);
        if !still_used.is_empty() && retire.is_empty() {
            tracing::info!(host = %host, image = %image, kept = ?still_used, "sandbox image ready on a host no longer kept prepared; previous images left in place and recorded");
        }
        for old in retire {
            match self
                .provider
                .remove_prepared_image_on(row.node_id, &old)
                .await
            {
                Ok(ImageRemoval::InUse) => {
                    tracing::info!(host = %host, image = %old, "previous sandbox image still in use; removal retried later");
                    still_used.push(old);
                }
                Ok(
                    ImageRemoval::Removed | ImageRemoval::NotPresent | ImageRemoval::Unsupported,
                ) => {}
                Err(error) => {
                    tracing::warn!(host = %host, image = %old, %error, "could not remove a previous sandbox image; retried later");
                    still_used.push(old);
                }
            }
        }
        let mut active = row.clone().into_active_model();
        active.prepared_image = Set(Some(image.to_string()));
        active.stale_images = Set(pending_removal(host, still_used));
        active.last_error = Set(None);
        active.last_error_at = Set(None);
        if let Err(error) = active.update(self.db.as_ref()).await {
            tracing::error!(host = %host, image = %image, %error, "could not record a prepared sandbox image");
        } else {
            tracing::info!(host = %host, image = %image, "sandbox image prepared");
        }
    }

    async fn record_failure(
        &self,
        host: &str,
        row: &sandbox_image_preparations::Model,
        reason: &str,
    ) {
        tracing::warn!(host = %host, reason = %reason, "sandbox image preparation failed");
        let mut active = row.clone().into_active_model();
        active.last_error = Set(Some(reason.to_string()));
        active.last_error_at = Set(Some(Utc::now()));
        if let Err(error) = active.update(self.db.as_ref()).await {
            tracing::error!(host = %host, %error, "could not record a failed sandbox image preparation");
        }
    }

    /// One pass over the hosts kept prepared: pull a changed default, record
    /// a pull that finished, retry an old image's removal.
    pub async fn reconcile(self: &Arc<Self>) -> Result<(), ImagePreparationError> {
        let rows: Vec<sandbox_image_preparations::Model> =
            sandbox_image_preparations::Entity::find()
                .filter(sandbox_image_preparations::Column::KeepPrepared.eq(true))
                .all(self.db.as_ref())
                .await
                .map_err(|source| ImagePreparationError::Database {
                    operation: "reading",
                    host: "hosts kept prepared".to_string(),
                    source,
                })?;
        if rows.is_empty() {
            return Ok(());
        }
        let hosts = self.hosts().await?;
        for row in rows {
            let id = api_node_id(row.node_id);
            let Some(host) = hosts.iter().find(|h| h.id == id) else {
                continue;
            };
            if !host.eligible {
                continue;
            }
            let status = match tokio::time::timeout(
                HOST_CALL_DEADLINE,
                self.provider
                    .image_prepare_status_on(row.node_id, None, None),
            )
            .await
            {
                Ok(Ok(status)) => status,
                Ok(Err(error)) => {
                    tracing::debug!(host = %host.name, %error, "sandbox image status unavailable during reconcile");
                    continue;
                }
                Err(_) => continue,
            };
            match plan_host(&row, &status, Utc::now()) {
                HostAction::Nothing => {
                    if !row.stale_images.is_empty() {
                        self.retry_removal(&host.name, &row).await;
                    }
                }
                HostAction::Prepare => {
                    tracing::info!(host = %host.name, image = %status.image, "sandbox image default changed or missing; preparing it");
                    self.start_host(&host.name, row).await;
                }
                HostAction::Watch { image } => self.spawn_watcher(host.name.clone(), row, image),
                HostAction::Record { image } => self.record_ready(&host.name, &row, &image).await,
            }
        }
        Ok(())
    }

    /// Try again to remove every image awaiting removal; keep the ones a
    /// container still uses. Only the images removed here leave the stored
    /// list (see [`subtract_removed_statement`]), so images another writer
    /// added while this pass waited on the host stay recorded.
    async fn retry_removal(&self, host: &str, row: &sandbox_image_preparations::Model) {
        let mut removed = Vec::new();
        for image in &row.stale_images {
            if row.prepared_image.as_deref() == Some(image.as_str()) {
                continue;
            }
            match self
                .provider
                .remove_prepared_image_on(row.node_id, image)
                .await
            {
                Ok(ImageRemoval::InUse) => {}
                Err(error) => {
                    tracing::debug!(host = %host, image = %image, %error, "could not remove a previous sandbox image; retried later");
                }
                Ok(
                    ImageRemoval::Removed | ImageRemoval::NotPresent | ImageRemoval::Unsupported,
                ) => {
                    tracing::info!(host = %host, image = %image, "previous sandbox image removed");
                    removed.push(image.clone());
                }
            }
        }
        if removed.is_empty() {
            return;
        }
        if let Err(error) = self
            .db
            .execute(subtract_removed_statement(row.id, removed))
            .await
        {
            tracing::warn!(host = %host, %error, "could not record removed sandbox images");
        }
    }

    /// Run [`Self::reconcile`] forever: once shortly after startup, then
    /// every [`RECONCILE_INTERVAL`].
    pub async fn run_reconciler(self: Arc<Self>) {
        tokio::time::sleep(RECONCILE_STARTUP_DELAY).await;
        loop {
            if let Err(error) = self.reconcile().await {
                tracing::warn!(%error, "sandbox image preparation pass failed");
            }
            tokio::time::sleep(RECONCILE_INTERVAL).await;
        }
    }
}

/// One host's entry: the live status for an eligible host, the row's
/// record for any.
async fn describe_host(
    provider: &dyn SandboxProvider,
    host: PlacementNode,
    row: Option<sandbox_image_preparations::Model>,
) -> HostImagePreparation {
    let node_id = row_node_id(host.id);
    let (status, status_error) = if host.eligible {
        match tokio::time::timeout(
            HOST_CALL_DEADLINE,
            provider.image_prepare_status_on(node_id, None, None),
        )
        .await
        {
            Ok(Ok(status)) => (Some(status), None),
            Ok(Err(error)) => (None, Some(error.to_string())),
            Err(_) => (
                None,
                Some(format!(
                    "host '{}' did not report its sandbox image status within {}s",
                    host.name,
                    HOST_CALL_DEADLINE.as_secs()
                )),
            ),
        }
    } else {
        (None, None)
    };
    let name = if host.is_control_plane && host.name.is_empty() {
        CONTROL_PLANE_NAME.to_string()
    } else {
        host.name
    };
    HostImagePreparation {
        node_id: host.id,
        node_name: name,
        is_control_plane: host.is_control_plane,
        eligible: host.eligible,
        ineligible_reason: (!host.eligible).then(|| host.reason.unwrap_or(host.status)),
        keep_prepared: row.as_ref().is_some_and(|r| r.keep_prepared),
        status,
        status_error,
        prepared_image: row.as_ref().and_then(|r| r.prepared_image.clone()),
        stale_images: row
            .as_ref()
            .map(|r| r.stale_images.clone())
            .unwrap_or_default(),
        last_error: row.as_ref().and_then(|r| r.last_error.clone()),
        last_error_at: row.as_ref().and_then(|r| r.last_error_at),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(keep: bool, prepared: Option<&str>) -> sandbox_image_preparations::Model {
        sandbox_image_preparations::Model {
            id: 1,
            node_id: None,
            keep_prepared: keep,
            prepared_image: prepared.map(str::to_string),
            stale_images: Vec::new(),
            last_error: None,
            last_error_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn status(image: &str, state: ImagePrepareState) -> ImagePrepareStatus {
        ImagePrepareStatus::new(image, state)
    }

    #[test]
    fn a_host_not_kept_prepared_is_never_touched() {
        for state in [
            ImagePrepareState::NotPulled,
            ImagePrepareState::Failed,
            ImagePrepareState::Ready,
        ] {
            assert_eq!(
                plan_host(&row(false, None), &status("img:2", state), Utc::now()),
                HostAction::Nothing,
                "{state:?}: nothing is pulled without the operator's say-so"
            );
        }
    }

    #[test]
    fn a_changed_default_triggers_a_new_pull() {
        // After an upgrade the host's default is img:2, which is not there.
        let kept = row(true, Some("img:1"));
        assert_eq!(
            plan_host(
                &kept,
                &status("img:2", ImagePrepareState::NotPulled),
                Utc::now()
            ),
            HostAction::Prepare
        );
        // A create pulled it first: record it (and retire img:1).
        assert_eq!(
            plan_host(
                &kept,
                &status("img:2", ImagePrepareState::Ready),
                Utc::now()
            ),
            HostAction::Record {
                image: "img:2".into()
            }
        );
        // Unchanged: nothing.
        assert_eq!(
            plan_host(
                &kept,
                &status("img:1", ImagePrepareState::Ready),
                Utc::now()
            ),
            HostAction::Nothing
        );
    }

    #[test]
    fn a_running_pull_is_followed_not_restarted() {
        assert_eq!(
            plan_host(
                &row(true, None),
                &status("img:1", ImagePrepareState::Pulling),
                Utc::now()
            ),
            HostAction::Watch {
                image: "img:1".into()
            }
        );
    }

    #[test]
    fn a_failure_is_retried_only_after_the_backoff() {
        let now = Utc::now();
        let mut failed = row(true, None);
        failed.last_error = Some("manifest unknown".into());
        failed.last_error_at = Some(now - chrono::Duration::minutes(5));
        let s = status("img:1", ImagePrepareState::Failed);
        assert_eq!(plan_host(&failed, &s, now), HostAction::Nothing);
        failed.last_error_at = Some(now - RETRY_FAILED_AFTER);
        assert_eq!(plan_host(&failed, &s, now), HostAction::Prepare);
    }

    #[test]
    fn an_unsupported_host_is_left_alone() {
        assert_eq!(
            plan_host(
                &row(true, None),
                &status("", ImagePrepareState::Unsupported),
                Utc::now()
            ),
            HostAction::Nothing
        );
    }

    #[test]
    fn retiring_never_removes_the_ready_image_and_dedupes() {
        let mut r = row(true, Some("img:1"));
        r.stale_images = vec!["img:0".into()];
        assert_eq!(images_to_retire(&r, "img:2"), vec!["img:1", "img:0"]);
        assert!(images_to_retire(&r, "img:1") == vec!["img:0".to_string()]);
        r.stale_images = vec!["img:1".into()];
        assert_eq!(images_to_retire(&r, "img:2"), vec!["img:1"]);
        assert!(images_to_retire(&row(true, None), "img:2").is_empty());
    }

    #[test]
    fn every_image_still_in_use_stays_tracked_across_upgrades() {
        // Two upgrades while sandboxes keep both older images in use: all of
        // them stay candidates for removal, none is forgotten.
        let mut r = row(true, Some("img:2"));
        r.stale_images = vec!["img:1".into(), "img:0".into()];
        let retire = images_to_retire(&r, "img:3");
        assert_eq!(retire, vec!["img:2", "img:1", "img:0"]);
        assert_eq!(
            pending_removal("control plane", retire),
            vec!["img:2", "img:1", "img:0"]
        );
    }

    #[test]
    fn a_host_no_longer_kept_prepared_keeps_every_older_image_recorded() {
        // The policy was turned off while an upgrade pull ran: nothing is
        // removed, and the image prepared before is not forgotten.
        let mut off = row(false, Some("img:1"));
        off.stale_images = vec!["img:0".into()];
        let (remove, keep) = outgoing_images(&off, "img:2");
        assert!(remove.is_empty());
        assert_eq!(keep, vec!["img:1", "img:0"]);

        let mut on = row(true, Some("img:1"));
        on.stale_images = vec!["img:0".into()];
        let (remove, keep) = outgoing_images(&on, "img:2");
        assert_eq!(remove, vec!["img:1", "img:0"]);
        assert!(keep.is_empty());
    }

    #[test]
    fn images_awaiting_removal_are_bounded() {
        let many: Vec<String> = (0..MAX_STALE_IMAGES + 5)
            .map(|i| format!("img:{i}"))
            .collect();
        let kept = pending_removal("control plane", many.clone());
        assert_eq!(kept.len(), MAX_STALE_IMAGES);
        assert_eq!(kept[..], many[..MAX_STALE_IMAGES], "the newest are kept");
    }

    #[test]
    fn the_control_plane_is_node_zero_in_the_api_and_null_in_the_table() {
        assert_eq!(api_node_id(None), 0);
        assert_eq!(api_node_id(Some(4)), 4);
        assert_eq!(row_node_id(0), None);
        assert_eq!(row_node_id(4), Some(4));
    }

    /// A host that accepts a prepare request and never answers it, counting
    /// how often it was asked and recording every image it was told to
    /// remove.
    #[derive(Default)]
    struct UnresponsiveHost {
        asked: std::sync::atomic::AtomicUsize,
        removed: std::sync::Mutex<Vec<String>>,
        /// Images a container still uses: their removal answers `InUse`.
        in_use: Vec<String>,
    }

    #[async_trait::async_trait]
    impl SandboxProvider for UnresponsiveHost {
        async fn prepare_image(
            &self,
            _image: Option<&str>,
            _backend: Option<temps_sandbox_providers::SandboxBackend>,
        ) -> Result<ImagePrepareStatus, temps_agent_core::error::AgentError> {
            self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::future::pending().await
        }
        async fn create(
            &self,
            _c: temps_sandbox_providers::SandboxCreateConfig,
        ) -> Result<temps_sandbox_providers::SandboxHandle, temps_agent_core::error::AgentError>
        {
            unreachable!("not exercised")
        }
        async fn exec(
            &self,
            _h: &temps_sandbox_providers::SandboxHandle,
            _c: Vec<String>,
            _e: std::collections::HashMap<String, String>,
            _o: Option<temps_agent_core::OnEventCallback>,
        ) -> Result<temps_sandbox_providers::SandboxExecResult, temps_agent_core::error::AgentError>
        {
            unreachable!("not exercised")
        }
        async fn is_alive(
            &self,
            _h: &temps_sandbox_providers::SandboxHandle,
        ) -> Result<bool, temps_agent_core::error::AgentError> {
            Ok(false)
        }
        async fn write_file(
            &self,
            _h: &temps_sandbox_providers::SandboxHandle,
            _p: &str,
            _c: &[u8],
            _m: u32,
        ) -> Result<(), temps_agent_core::error::AgentError> {
            Ok(())
        }
        async fn read_file(
            &self,
            _h: &temps_sandbox_providers::SandboxHandle,
            _p: &str,
        ) -> Result<Vec<u8>, temps_agent_core::error::AgentError> {
            Ok(Vec::new())
        }
        async fn write_directory(
            &self,
            _h: &temps_sandbox_providers::SandboxHandle,
            _l: &std::path::Path,
            _t: &str,
        ) -> Result<(), temps_agent_core::error::AgentError> {
            Ok(())
        }
        async fn kill_processes(
            &self,
            _h: &temps_sandbox_providers::SandboxHandle,
            _p: &str,
            _s: temps_sandbox_providers::KillSignal,
        ) -> Result<(), temps_agent_core::error::AgentError> {
            Ok(())
        }
        async fn destroy(
            &self,
            _h: &temps_sandbox_providers::SandboxHandle,
            _p: bool,
        ) -> Result<(), temps_agent_core::error::AgentError> {
            Ok(())
        }
        async fn recover(
            &self,
            _run_id: i32,
        ) -> Result<
            Option<temps_sandbox_providers::SandboxHandle>,
            temps_agent_core::error::AgentError,
        > {
            Ok(None)
        }
        fn name(&self) -> &str {
            "unresponsive"
        }
        async fn is_available(&self) -> bool {
            true
        }
        async fn image_status(
            &self,
        ) -> Result<(bool, String), temps_agent_core::error::AgentError> {
            Ok((false, String::new()))
        }
        async fn rebuild_image(&self) -> Result<String, temps_agent_core::error::AgentError> {
            Ok(String::new())
        }
        async fn remove_prepared_image(
            &self,
            image: &str,
        ) -> Result<ImageRemoval, temps_agent_core::error::AgentError> {
            self.removed.lock().unwrap().push(image.to_string());
            if self.in_use.iter().any(|used| used == image) {
                return Ok(ImageRemoval::InUse);
            }
            Ok(ImageRemoval::Removed)
        }
    }

    /// Run a removal pass over `row` against a mock database and return the
    /// statements it wrote.
    async fn removal_pass_writes(
        row: sandbox_image_preparations::Model,
        host: Arc<UnresponsiveHost>,
    ) -> Vec<sea_orm::Transaction> {
        use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_results([MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }])
                .into_connection(),
        );
        let sandbox_service = Arc::new(
            crate::services::sandbox_service::tests::preview_test_service_with_db(db.clone()),
        );
        let service = ImagePreparationService::new(db.clone(), host, sandbox_service);
        service.retry_removal("control plane", &row).await;
        drop(service);
        Arc::try_unwrap(db)
            .unwrap_or_else(|_| panic!("the service still holds the database"))
            .into_transaction_log()
    }

    #[tokio::test]
    async fn a_removal_pass_takes_out_only_what_it_removed_from_the_stored_list() {
        // img:1 is still used, img:0 goes. The pass must not write back the
        // list it read (which would drop anything a finishing pull added
        // while it waited on the host): it subtracts img:0 from the list as
        // stored at write time.
        let host = Arc::new(UnresponsiveHost {
            in_use: vec!["img:1".into()],
            ..Default::default()
        });
        let mut before = row(true, Some("img:2"));
        before.stale_images = vec!["img:1".into(), "img:0".into()];

        let writes = removal_pass_writes(before, host.clone()).await;

        assert_eq!(*host.removed.lock().unwrap(), vec!["img:1", "img:0"]);
        assert_eq!(
            writes,
            vec![sea_orm::Transaction::one(subtract_removed_statement(
                1,
                vec!["img:0".into()]
            ))]
        );
        let sql = writes[0].statements()[0].sql.clone();
        assert!(
            sql.contains("unnest(stale_images)") && sql.contains("ANY($2)"),
            "{sql}"
        );
    }

    #[tokio::test]
    async fn a_removal_pass_that_removed_nothing_writes_nothing() {
        let host = Arc::new(UnresponsiveHost {
            in_use: vec!["img:0".into()],
            ..Default::default()
        });
        let mut before = row(true, Some("img:2"));
        before.stale_images = vec!["img:0".into()];

        assert!(removal_pass_writes(before, host).await.is_empty());
    }

    /// The service over a mock database that answers `results` in order.
    fn service_with(
        results: Vec<Vec<sandbox_image_preparations::Model>>,
        host: Arc<UnresponsiveHost>,
    ) -> Arc<ImagePreparationService> {
        use sea_orm::{DatabaseBackend, MockDatabase};
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(results)
                .into_connection(),
        );
        let sandbox_service = Arc::new(
            crate::services::sandbox_service::tests::preview_test_service_with_db(db.clone()),
        );
        Arc::new(ImagePreparationService::new(db, host, sandbox_service))
    }

    #[tokio::test]
    async fn a_ready_image_retires_the_previous_one_on_a_host_kept_prepared() {
        let host = Arc::new(UnresponsiveHost::default());
        let mut before = row(true, Some("img:1"));
        before.stale_images = vec!["img:0".into()];
        let service = service_with(vec![vec![row(true, Some("img:2"))]], host.clone());

        service
            .record_ready("control plane", &before, "img:2")
            .await;

        assert_eq!(*host.removed.lock().unwrap(), vec!["img:1", "img:0"]);
    }

    #[tokio::test]
    async fn a_ready_image_removes_nothing_once_keep_prepared_is_off() {
        // The operator turned keep_prepared off while the pull ran: the
        // result is recorded, the older images and the removal list stay.
        let host = Arc::new(UnresponsiveHost::default());
        let mut before = row(false, Some("img:1"));
        before.stale_images = vec!["img:0".into()];
        let service = service_with(vec![vec![row(false, Some("img:2"))]], host.clone());

        service
            .record_ready("control plane", &before, "img:2")
            .await;

        assert!(
            host.removed.lock().unwrap().is_empty(),
            "removed {:?}",
            host.removed.lock().unwrap()
        );
    }

    #[tokio::test]
    async fn prepare_saves_the_policy_and_returns_without_waiting_for_hosts() {
        use sea_orm::{DatabaseBackend, MockDatabase};
        let saved = row(true, None);
        // Placement (settings, workers, live counts), then the upsert inside
        // the transaction. A slow host is never waited for.
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<temps_entities::settings::Model>::new()])
                .append_query_results([Vec::<temps_entities::nodes::Model>::new()])
                .append_query_results([Vec::<temps_entities::sandboxes::Model>::new()])
                .append_query_results([vec![saved]])
                .into_connection(),
        );
        let sandbox_service = Arc::new(
            crate::services::sandbox_service::tests::preview_test_service_with_db(db.clone()),
        );
        let host = Arc::new(UnresponsiveHost::default());
        let service = Arc::new(ImagePreparationService::new(
            db,
            host.clone(),
            sandbox_service,
        ));

        let prepared = tokio::time::timeout(Duration::from_secs(1), service.prepare(None))
            .await
            .expect("prepare returns without waiting for the host")
            .expect("prepare");
        assert_eq!(prepared, vec![0], "the control plane");

        // The host is asked in the background.
        for _ in 0..100 {
            if host.asked.load(std::sync::atomic::Ordering::SeqCst) == 1 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("the host was never asked to pull");
    }
}
