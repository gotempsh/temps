// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::{
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter};
use serde::Serialize;
use temps_cloud_client::{BackendUrl, CloudError, CloudFeatureSwitches, CloudLink, EnrollmentKind};
use temps_cloud_protocol::{
    ManagedBackupCapability, ManagedNotificationAccepted, ManagedNotificationRequest,
};
use temps_config::{ConfigService, ConfigServiceError};
use temps_core::EncryptionService;
use thiserror::Error;
use tokio::sync::{watch, Mutex as AsyncMutex};
use utoipa::ToSchema;
use uuid::Uuid;

const SETUP_PATH: &str = "/settings/cloud";
const SHUTDOWN_TASK_TIMEOUT: Duration = Duration::from_secs(6);
/// Name given to the auto-provisioned Cloud-managed `s3_sources` row.
/// Distinct from any operator-chosen name so it is unmistakable in the UI.
const MANAGED_BACKUP_SOURCE_NAME: &str = "Temps Cloud managed backups";

#[derive(Debug, Error)]
pub enum CloudServiceError {
    #[error("Could not read managed-control-plane settings: {0}")]
    Configuration(#[from] ConfigServiceError),
    #[error("Managed-control-plane URL is invalid: {reason}")]
    InvalidBackend { reason: String },
    #[error("Managed-control-plane operation failed: {0}")]
    Client(CloudError),
    #[error("Could not persist the managed-control-plane link: {0}")]
    State(temps_cloud_client::state::StateError),
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),
    #[error("Could not persist the managed Cloud backup credential: {0}")]
    ManagedBackupCredential(#[from] temps_entities::s3_sources::S3SourceCredentialError),
}

/// Outcome of attempting to provision a Cloud-managed backup source as part
/// of [`CloudService::enroll`]. Never turns enrollment itself into a failure
/// — a tenant without managed backups, or a backend that is not ready yet,
/// still gets a fully linked instance.
#[derive(Debug, Clone, PartialEq)]
pub enum ManagedBackupOutcome {
    /// Cloud reported `configured: false` (tier does not include managed
    /// backups, or the backend has not provisioned one yet).
    NotConfigured { reason: Option<String> },
    /// A Cloud-managed `s3_sources` row now exists (inserted, or rotated
    /// in place against the same bucket it already pointed at).
    Provisioned,
    /// Cloud returned a *different* `bucket_name` than the one already on
    /// record for this instance's managed source. The credential was still
    /// rotated in place — refusing would strand the instance without a
    /// usable managed source — but this should never happen under a correct
    /// backend contract (the tenant->bucket mapping must be a stable 1:1
    /// record, established once): it either means an unannounced storage
    /// migration on Cloud's side, or a backend bug re-provisioning a fresh
    /// bucket per call. Either way, backups already written under the
    /// previous bucket are now orphaned from this instance's `s3_sources`
    /// row and invisible in the UI, so this is surfaced loudly rather than
    /// treated as a routine rotation.
    ProvisionedBucketChanged {
        previous_bucket_name: String,
        new_bucket_name: String,
    },
    /// The capability call or the local persistence failed. Enrollment still
    /// succeeded; the reason is logged and available here for the caller to
    /// decide whether to surface it.
    Unavailable(String),
}

/// Outcome of upserting the local `managed_by_cloud` row, distinguishing a
/// routine credential rotation from one that landed on a different bucket
/// than what was already on record (see [`ManagedBackupOutcome::ProvisionedBucketChanged`]).
enum UpsertOutcome {
    SameBucket,
    BucketChanged {
        previous_bucket_name: String,
        new_bucket_name: String,
    },
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CloudCapability {
    pub configured: bool,
    pub reason: Option<String>,
    pub setup_path: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CloudAiCapability {
    pub configured: bool,
    pub reason: Option<String>,
    pub setup_path: String,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ManagedBackupSetupStatus {
    Disabled,
    Ready,
    NeedsSetup,
    SubscriptionRequired,
    Unavailable,
}

#[derive(Debug, Clone, Copy, Serialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ManagedBackupSetupAction {
    None,
    Retry,
    RenewSubscription,
}

#[derive(Debug, Clone, Serialize, ToSchema, PartialEq, Eq)]
pub struct ManagedBackupSetup {
    pub status: ManagedBackupSetupStatus,
    pub ready: bool,
    pub message: String,
    pub action: ManagedBackupSetupAction,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CloudStatus {
    pub status: String,
    pub status_message: String,
    pub health: String,
    pub health_message: String,
    #[schema(value_type = Option<String>)]
    pub instance_id: Option<Uuid>,
    pub account_email: Option<String>,
    pub spooled_spans: usize,
    pub backend_url: String,
    pub telemetry_enabled: bool,
    pub backups_enabled: bool,
    pub notifications_enabled: bool,
    pub managed_backup_setup: ManagedBackupSetup,
}

pub struct CloudService {
    link: Arc<CloudLink>,
    config: Arc<ConfigService>,
    db: Arc<DatabaseConnection>,
    encryption: Arc<EncryptionService>,
    cancel: watch::Sender<bool>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    backup_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    backup_credential_rotation_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    heartbeat_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    allow_loopback_development: bool,
    configuration_issue: RwLock<Option<String>>,
    managed_backup_setup: RwLock<Option<ManagedBackupSetup>>,
    managed_backup_reconcile_lock: AsyncMutex<()>,
}

impl CloudService {
    pub fn new(
        link: Arc<CloudLink>,
        config: Arc<ConfigService>,
        db: Arc<DatabaseConnection>,
        encryption: Arc<EncryptionService>,
        allow_loopback_development: bool,
    ) -> Self {
        let (cancel, _) = watch::channel(false);
        Self {
            link,
            config,
            db,
            encryption,
            cancel,
            task: Mutex::new(None),
            backup_task: Mutex::new(None),
            backup_credential_rotation_task: Mutex::new(None),
            heartbeat_task: Mutex::new(None),
            allow_loopback_development,
            configuration_issue: RwLock::new(None),
            managed_backup_setup: RwLock::new(None),
            managed_backup_reconcile_lock: AsyncMutex::new(()),
        }
    }

    fn set_configuration_issue(&self, issue: Option<String>) {
        *self
            .configuration_issue
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = issue;
    }

    fn configuration_issue(&self) -> Option<String> {
        self.configuration_issue
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn set_managed_backup_setup(&self, setup: ManagedBackupSetup) {
        *self
            .managed_backup_setup
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(setup);
    }

    fn managed_backup_setup(&self) -> Option<ManagedBackupSetup> {
        self.managed_backup_setup
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn start_flusher(&self) {
        let mut task = self
            .task
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if task.is_none() {
            let link = self.link.clone();
            let cancel = self.cancel.subscribe();
            *task = Some(tokio::spawn(async move {
                temps_cloud_client::flusher::run(link, cancel).await;
            }));
        }
    }

    pub fn start_backup_mirror(
        &self,
        db: Arc<sea_orm::DatabaseConnection>,
        encryption: Arc<temps_core::EncryptionService>,
    ) {
        let mut task = self
            .backup_task
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if task.is_none() {
            tracing::info!("Cloud service launching backup mirror task");
            let link = self.link.clone();
            let cancel = self.cancel.subscribe();
            *task = Some(tokio::spawn(async move {
                crate::backup_mirror::run(link, db, encryption, cancel).await;
            }));
        } else {
            tracing::debug!("Cloud backup mirror task is already registered");
        }
    }

    /// Launch the background loop that keeps the Cloud-managed backup
    /// credential from ever going stale. Cloud-issued credentials are
    /// ephemeral — expiring at least daily by contract — so a linked
    /// instance that only ever provisioned once at enroll time would start
    /// silently failing every backup once that credential expired. This
    /// re-fetches and rotates it well inside that window regardless.
    pub fn start_backup_credential_rotation(self: &Arc<Self>) {
        let mut task = self
            .backup_credential_rotation_task
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if task.is_none() {
            tracing::info!("Cloud service launching backup credential rotation task");
            let service = self.clone();
            let cancel = self.cancel.subscribe();
            *task = Some(tokio::spawn(async move {
                crate::backup_credential_rotation::run(service, cancel).await;
            }));
        } else {
            tracing::debug!("Cloud backup credential rotation task is already registered");
        }
    }

    /// Launch the heartbeat sender: a dedicated liveness signal on the
    /// management channel, independent of whatever telemetry or backups this
    /// instance does or does not have to ship. Like the backup mirror, it is
    /// safe to spawn unconditionally at startup -- it self-gates on
    /// [`temps_cloud_client::CloudLink::is_linked`] every cycle, so it starts
    /// working the moment the instance links and goes quiet (cheaply) the
    /// moment it disconnects, with no separate start/stop wiring needed.
    pub fn start_heartbeat_sender(&self) {
        let mut task = self
            .heartbeat_task
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if task.is_none() {
            tracing::info!("Cloud service launching heartbeat sender task");
            let link = self.link.clone();
            let cancel = self.cancel.subscribe();
            *task = Some(tokio::spawn(async move {
                temps_cloud_client::heartbeat::run(link, cancel).await;
            }));
        } else {
            tracing::debug!("Cloud heartbeat sender task is already registered");
        }
    }

    pub fn link(&self) -> Arc<CloudLink> {
        self.link.clone()
    }

    /// Apply explicit persisted operator consent. Enrollment does not call
    /// this method and therefore cannot enable exports by itself.
    pub fn set_feature_switches(
        &self,
        switches: CloudFeatureSwitches,
    ) -> Result<(), CloudServiceError> {
        self.link
            .set_feature_switches(switches)
            .map_err(CloudServiceError::State)
    }

    pub fn feature_switches(&self) -> CloudFeatureSwitches {
        self.link.feature_switches()
    }

    /// Tell the link whether a Cloud-managed backup destination exists locally.
    ///
    /// The backup mirror keys off this rather than off the export consent
    /// switch: once `s3_sources.managed_by_cloud` exists, completed backups are
    /// physically written into a Cloud-owned bucket, and only the mirror can
    /// give Cloud a record of them. Refreshed from the row itself, which is the
    /// single source of truth, rather than inferred from settings.
    async fn refresh_managed_backup_destination(&self) {
        match self.has_managed_backup_source().await {
            Ok(present) => self.link.set_managed_backup_destination(present),
            Err(error) => tracing::error!(
                %error,
                "could not determine whether a Cloud-managed backup destination exists; \
                 leaving backup mirroring gated on the export consent switch"
            ),
        }
    }

    pub async fn initialize(&self) -> Result<(), CloudServiceError> {
        // Before any early return below: a degraded Cloud configuration must
        // not make the instance forget that its backups are already landing in
        // a Cloud-managed bucket.
        self.refresh_managed_backup_destination().await;
        let settings = match self.config.get_settings().await {
            Ok(settings) => settings,
            Err(error) => {
                tracing::error!(%error, "Cloud settings unavailable; Cloud integration disabled");
                if let Err(state_error) = self
                    .link
                    .set_feature_switches(CloudFeatureSwitches::default())
                {
                    tracing::error!(%state_error, "could not purge Cloud telemetry while disabling integration");
                }
                self.link
                    .block_outbound("Cloud settings are unavailable; fix local settings and retry");
                self.set_configuration_issue(Some(
                    "Cloud settings could not be loaded. Check the server logs, then retry."
                        .to_string(),
                ));
                self.start_flusher();
                return Ok(());
            }
        };
        if let Err(state_error) = self.link.set_feature_switches(CloudFeatureSwitches {
            telemetry: settings.cloud.telemetry_enabled,
            backups: settings.cloud.backups_enabled,
            notifications: settings.cloud.notifications_enabled,
        }) {
            tracing::error!(%state_error, "Cloud consent state could not be applied; outbound operations blocked");
            self.link.block_outbound(
                "telemetry consent state could not be persisted; repair the Cloud link state",
            );
            self.set_configuration_issue(Some(
                "Cloud telemetry consent could not be persisted. Check the server logs before reconnecting."
                    .to_string(),
            ));
            self.start_flusher();
            return Ok(());
        }
        let backend = match parse_backend(
            &settings.cloud.backend_url,
            self.allow_loopback_development || self.link.allows_loopback_development(),
        ) {
            Ok(backend) => backend,
            Err(error) => {
                tracing::error!(%error, "Cloud backend configuration invalid; Cloud integration disabled");
                if let Err(state_error) = self
                    .link
                    .set_feature_switches(CloudFeatureSwitches::default())
                {
                    tracing::error!(%state_error, "could not purge Cloud telemetry while disabling invalid integration");
                }
                self.link
                    .block_outbound("the configured backend URL is invalid; update Cloud settings");
                self.set_configuration_issue(Some(format!(
                    "Cloud backend configuration is invalid: {error}. Update it in Cloud settings."
                )));
                self.start_flusher();
                return Ok(());
            }
        };
        if let Err(error) = self.link.configure(backend) {
            if matches!(
                error,
                temps_cloud_client::state::StateError::UnreadableStateBlocksMutation { .. }
            ) {
                tracing::error!(%error, "Cloud service started with unreadable link state");
            } else {
                tracing::error!(%error, "Cloud link configuration could not be applied; Cloud integration disabled");
                if let Err(state_error) = self
                    .link
                    .set_feature_switches(CloudFeatureSwitches::default())
                {
                    tracing::error!(%state_error, "could not purge Cloud telemetry after configuration failure");
                }
                self.link.block_outbound(
                    "the configured Cloud link could not be applied; check server logs",
                );
                self.set_configuration_issue(Some(
                    "Cloud link configuration could not be applied. Check the server logs and Cloud settings."
                        .to_string(),
                ));
            }
        } else {
            self.set_configuration_issue(None);
        }
        self.start_flusher();
        Ok(())
    }

    pub async fn capability(&self) -> CloudCapability {
        match self.config.get_settings().await {
            Ok(settings) => {
                match parse_backend(
                    &settings.cloud.backend_url,
                    self.allow_loopback_development || self.link.allows_loopback_development(),
                ) {
                    Ok(_) => CloudCapability {
                        configured: true,
                        reason: None,
                        setup_path: SETUP_PATH.to_string(),
                    },
                    Err(error) => CloudCapability {
                        configured: false,
                        reason: Some(error.to_string()),
                        setup_path: SETUP_PATH.to_string(),
                    },
                }
            }
            Err(error) => CloudCapability {
                configured: false,
                reason: Some(format!("Could not load settings: {error}")),
                setup_path: SETUP_PATH.to_string(),
            },
        }
    }

    pub async fn status(&self) -> Result<CloudStatus, CloudServiceError> {
        let (backend_url, settings_issue) = match self.config.get_settings().await {
            Ok(settings) => (settings.cloud.backend_url, None),
            Err(error) => {
                tracing::error!(%error, "Could not refresh Cloud settings for status");
                (
                    String::new(),
                    Some(
                        "Cloud settings could not be loaded. Check the server logs, then retry."
                            .to_string(),
                    ),
                )
            }
        };
        let link_status = self.link.status();
        let issue = settings_issue.or_else(|| self.configuration_issue());
        let (status, status_message) = if matches!(
            link_status,
            temps_cloud_client::LinkStatus::StateUnreadable { .. }
        ) {
            (status_name(&link_status).to_string(), link_status.message())
        } else if let Some(issue) = issue {
            ("configuration_invalid".to_string(), issue)
        } else {
            (status_name(&link_status).to_string(), link_status.message())
        };
        let health = self.link.health();
        let switches = self.link.feature_switches();
        let managed_backup_setup = if let Some(setup) = self.managed_backup_setup() {
            setup
        } else if self.has_managed_backup_source().await? {
            ready_managed_backup_setup()
        } else {
            default_managed_backup_setup(switches.backups)
        };
        Ok(CloudStatus {
            status,
            status_message,
            health: health_name(&health).to_string(),
            health_message: health.message(),
            instance_id: self.link.instance_id(),
            account_email: self.link.account_email(),
            spooled_spans: self.link.spooled(),
            backend_url,
            telemetry_enabled: switches.telemetry,
            backups_enabled: switches.backups,
            notifications_enabled: switches.notifications,
            managed_backup_setup,
        })
    }

    pub async fn ai_capability(&self) -> Result<CloudAiCapability, CloudServiceError> {
        match self.link.managed_ai_capability().await {
            Ok(capability) => Ok(CloudAiCapability {
                configured: capability.configured,
                reason: capability.reason,
                setup_path: capability.setup_path,
                model: capability.managed_model,
            }),
            Err(CloudError::NotEnrolled) => Ok(CloudAiCapability {
                configured: false,
                reason: Some("Link this instance to use managed AI.".to_string()),
                setup_path: SETUP_PATH.to_string(),
                model: None,
            }),
            Err(error) => Err(CloudServiceError::Client(error)),
        }
    }

    pub async fn update_feature_switches(
        &self,
        switches: CloudFeatureSwitches,
    ) -> Result<CloudStatus, CloudServiceError> {
        self.config
            .update_cloud_features(switches.telemetry, switches.backups, switches.notifications)
            .await?;
        if let Err(error) = self.link.set_feature_switches(switches) {
            self.link.block_outbound(
                "telemetry consent state could not be persisted; repair the Cloud link state",
            );
            return Err(CloudServiceError::State(error));
        }
        if switches.backups {
            self.reconcile_managed_backup_source().await?;
        } else {
            self.set_managed_backup_setup(default_managed_backup_setup(false));
        }
        self.status().await
    }

    /// Enroll this instance and, if the tenant's plan includes it, provision
    /// a Cloud-managed offsite backup destination. Backup provisioning never
    /// fails enrollment itself — see [`ManagedBackupOutcome`].
    ///
    /// The returned [`EnrollmentKind`] says whether this call *established* the
    /// link or merely re-authenticated an existing one. It is passed straight
    /// through from [`CloudLink::enroll`], which is the only place that can
    /// still see the prior credential, and it exists so a caller with a side
    /// effect that belongs to linking — ADR-042's purchase-triggered telemetry
    /// activation — does not fire it on an ordinary credential recovery.
    pub async fn enroll(
        &self,
        code: &str,
    ) -> Result<(CloudStatus, ManagedBackupOutcome, EnrollmentKind), CloudServiceError> {
        let settings = self.config.get_settings().await?;
        let backend = parse_backend(
            &settings.cloud.backend_url,
            self.allow_loopback_development || self.link.allows_loopback_development(),
        )
        .map_err(|error| CloudServiceError::InvalidBackend {
            reason: error.to_string(),
        })?;
        self.link
            .configure(backend)
            .map_err(CloudServiceError::State)?;
        self.set_configuration_issue(None);
        self.link
            .set_feature_switches(CloudFeatureSwitches {
                telemetry: settings.cloud.telemetry_enabled,
                backups: settings.cloud.backups_enabled,
                notifications: settings.cloud.notifications_enabled,
            })
            .map_err(CloudServiceError::State)?;
        let enrollment = self
            .link
            .enroll(code)
            .await
            .map_err(CloudServiceError::Client)?;
        let backup_outcome = self.provision_managed_backup_source().await;
        self.set_managed_backup_setup(managed_backup_setup_from_outcome(&backup_outcome));
        let status = self.status().await?;
        Ok((status, backup_outcome, enrollment))
    }

    /// Fetch (or refresh) the tenant's managed backup credential and upsert
    /// it into `s3_sources`. Failures are logged and returned as
    /// [`ManagedBackupOutcome::Unavailable`] rather than propagated — a
    /// linked instance must never lose its link over this.
    pub(crate) async fn provision_managed_backup_source(&self) -> ManagedBackupOutcome {
        let capability = match self.link.managed_backup_credentials().await {
            Ok(capability) => capability,
            Err(error) => {
                tracing::warn!(
                    %error,
                    "could not fetch a managed Cloud backup credential; continuing without one"
                );
                return ManagedBackupOutcome::Unavailable(error.to_string());
            }
        };
        if !capability.configured {
            let reason = capability.reason;
            tracing::info!(
                reason = reason.as_deref().unwrap_or("no reason given"),
                "Temps Cloud did not provision a managed backup destination for this tenant"
            );
            return ManagedBackupOutcome::NotConfigured { reason };
        }
        let credentials = match managed_backup_credentials_from_capability(capability) {
            Ok(credentials) => credentials,
            Err(reason) => {
                tracing::error!(
                    reason,
                    "managed backend reported configured=true but omitted required backup credential fields"
                );
                return ManagedBackupOutcome::Unavailable(reason);
            }
        };
        let upserted = self.upsert_managed_backup_source(credentials).await;
        if upserted.is_ok() {
            // The destination now exists, so every completed backup written to
            // it must be declared to Cloud from here on.
            self.link.set_managed_backup_destination(true);
        }
        match upserted {
            Ok(UpsertOutcome::SameBucket) => ManagedBackupOutcome::Provisioned,
            Ok(UpsertOutcome::BucketChanged {
                previous_bucket_name,
                new_bucket_name,
            }) => {
                tracing::error!(
                    previous_bucket_name,
                    new_bucket_name,
                    "Temps Cloud returned a different bucket for this tenant's managed backup \
                     source than the one already on record — the tenant->bucket mapping should \
                     be stable. Rotated the credential in place, but any backups already written \
                     to the previous bucket are now orphaned from this instance's S3 Sources list."
                );
                ManagedBackupOutcome::ProvisionedBucketChanged {
                    previous_bucket_name,
                    new_bucket_name,
                }
            }
            Err(error) => {
                tracing::error!(%error, "failed to persist the Cloud-managed backup source");
                ManagedBackupOutcome::Unavailable(error.to_string())
            }
        }
    }

    async fn upsert_managed_backup_source(
        &self,
        credentials: temps_entities::s3_sources::S3SourceCredentials,
    ) -> Result<UpsertOutcome, CloudServiceError> {
        let existing = temps_entities::s3_sources::Entity::find()
            .filter(temps_entities::s3_sources::Column::ManagedByCloud.eq(true))
            .one(self.db.as_ref())
            .await?;
        match existing {
            Some(row) => {
                let previous_bucket_name = row.bucket_name.clone();
                let bucket_changed = previous_bucket_name != credentials.bucket_name;
                let new_bucket_name = credentials.bucket_name.clone();
                temps_entities::s3_sources::update_encrypted(
                    self.db.as_ref(),
                    &self.encryption,
                    row.id,
                    credentials,
                )
                .await?;
                Ok(if bucket_changed {
                    UpsertOutcome::BucketChanged {
                        previous_bucket_name,
                        new_bucket_name,
                    }
                } else {
                    UpsertOutcome::SameBucket
                })
            }
            None => {
                let existing_count = temps_entities::s3_sources::Entity::find()
                    .count(self.db.as_ref())
                    .await?;
                temps_entities::s3_sources::insert_encrypted(
                    self.db.as_ref(),
                    &self.encryption,
                    credentials,
                    existing_count == 0,
                    true,
                )
                .await?;
                Ok(UpsertOutcome::SameBucket)
            }
        }
    }

    /// Disconnect this instance and remove any Cloud-managed backup source.
    /// Returns whether a managed source was found and removed, so the caller
    /// can decide whether to audit a credential revocation.
    pub async fn disconnect(&self) -> Result<(CloudStatus, bool), CloudServiceError> {
        match self.link.revoke().await {
            Ok(()) | Err(CloudError::CredentialRejected) => {}
            Err(error) => return Err(CloudServiceError::Client(error)),
        }
        self.link.disconnect().map_err(CloudServiceError::State)?;
        let removed = self.remove_managed_backup_source().await?;
        self.set_managed_backup_setup(default_managed_backup_setup(false));
        let status = self.status().await?;
        Ok((status, removed))
    }

    async fn has_managed_backup_source(&self) -> Result<bool, CloudServiceError> {
        Ok(temps_entities::s3_sources::Entity::find()
            .filter(temps_entities::s3_sources::Column::ManagedByCloud.eq(true))
            .count(self.db.as_ref())
            .await?
            > 0)
    }

    pub async fn reconcile_managed_backup_source(
        &self,
    ) -> Result<ManagedBackupSetup, CloudServiceError> {
        let _guard = self.managed_backup_reconcile_lock.lock().await;
        if !self.link.feature_switches().backups {
            let setup = default_managed_backup_setup(false);
            self.set_managed_backup_setup(setup.clone());
            return Ok(setup);
        }

        let outcome = self.provision_managed_backup_source().await;
        let setup = managed_backup_setup_from_outcome(&outcome);
        self.set_managed_backup_setup(setup.clone());
        Ok(setup)
    }

    /// Remove the Cloud-managed `s3_sources` row created by
    /// [`CloudService::enroll`], bypassing the user-facing `is_default` and
    /// `managed_by_cloud` guards in `temps-backup` — this is a system
    /// cleanup, not a user action. Still refuses to delete a row still
    /// referenced by backup schedules or retained backup records: `s3_sources`
    /// has an `ON DELETE CASCADE` foreign key from both, so deleting it out
    /// from under an operator's backup history on disconnect would silently
    /// destroy that history instead of merely removing a credential.
    async fn remove_managed_backup_source(&self) -> Result<bool, CloudServiceError> {
        let Some(source) = temps_entities::s3_sources::Entity::find()
            .filter(temps_entities::s3_sources::Column::ManagedByCloud.eq(true))
            .one(self.db.as_ref())
            .await?
        else {
            return Ok(false);
        };

        let schedule_count = temps_entities::backup_schedules::Entity::find()
            .filter(temps_entities::backup_schedules::Column::S3SourceId.eq(source.id))
            .count(self.db.as_ref())
            .await?;
        let backup_count = temps_entities::backups::Entity::find()
            .filter(temps_entities::backups::Column::S3SourceId.eq(source.id))
            .count(self.db.as_ref())
            .await?;
        if schedule_count > 0 || backup_count > 0 {
            tracing::warn!(
                s3_source_id = source.id,
                schedule_count,
                backup_count,
                "Cloud-managed S3 source is still referenced by backup schedules or records; \
                 leaving it in place on disconnect instead of cascading away backup history. \
                 Remove the schedules, then delete the source manually."
            );
            return Ok(false);
        }

        temps_entities::s3_sources::Entity::delete_by_id(source.id)
            .exec(self.db.as_ref())
            .await?;
        // Nothing of this instance's is in Cloud storage any more, so the
        // mirror goes back to being gated purely on operator consent.
        self.link.set_managed_backup_destination(false);
        Ok(true)
    }

    pub async fn send_notification(
        &self,
        request: &ManagedNotificationRequest,
    ) -> Result<ManagedNotificationAccepted, CloudServiceError> {
        self.link
            .send_notification(request)
            .await
            .map_err(CloudServiceError::Client)
    }

    pub async fn shutdown(&self) {
        let _ = self.cancel.send(true);
        let task = self
            .task
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(task) = task {
            await_task_shutdown(task, "managed telemetry mirror", SHUTDOWN_TASK_TIMEOUT).await;
        }
        let backup_task = self
            .backup_task
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(task) = backup_task {
            await_task_shutdown(task, "Cloud backup mirror", SHUTDOWN_TASK_TIMEOUT).await;
        }
        let rotation_task = self
            .backup_credential_rotation_task
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(task) = rotation_task {
            await_task_shutdown(
                task,
                "Cloud backup credential rotation",
                SHUTDOWN_TASK_TIMEOUT,
            )
            .await;
        }
        let heartbeat_task = self
            .heartbeat_task
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(task) = heartbeat_task {
            await_task_shutdown(task, "Cloud heartbeat sender", SHUTDOWN_TASK_TIMEOUT).await;
        }
    }
}

async fn await_task_shutdown(
    mut task: tokio::task::JoinHandle<()>,
    task_name: &'static str,
    timeout: Duration,
) {
    match tokio::time::timeout(timeout, &mut task).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::warn!(%error, task_name, "Cloud task did not shut down cleanly"),
        Err(_) => {
            tracing::warn!(
                task_name,
                timeout_ms = timeout.as_millis(),
                "Cloud task exceeded shutdown deadline; cancelling in-flight network work"
            );
            task.abort();
            let _ = task.await;
        }
    }
}

/// Validate that a `configured: true` capability actually carries every
/// field required to build a usable S3 source, and translate it. `endpoint`
/// alone stays optional — some providers (plain AWS S3) don't need one.
fn managed_backup_credentials_from_capability(
    capability: ManagedBackupCapability,
) -> Result<temps_entities::s3_sources::S3SourceCredentials, String> {
    let region = capability
        .region
        .ok_or_else(|| "managed backend response was missing `region`".to_string())?;
    let bucket_name = capability
        .bucket_name
        .ok_or_else(|| "managed backend response was missing `bucket_name`".to_string())?;
    let bucket_path = capability
        .bucket_path
        .ok_or_else(|| "managed backend response was missing `bucket_path`".to_string())?;
    let access_key_id = capability
        .access_key_id
        .ok_or_else(|| "managed backend response was missing `access_key_id`".to_string())?;
    let secret_key = capability
        .secret_key
        .ok_or_else(|| "managed backend response was missing `secret_key`".to_string())?;
    Ok(temps_entities::s3_sources::S3SourceCredentials {
        name: MANAGED_BACKUP_SOURCE_NAME.to_string(),
        bucket_name,
        bucket_path,
        access_key_id,
        secret_key,
        // Not required: a backend may vend a long-lived credential, and one
        // that predates the temporary-credential protocol sends neither field
        // at all. Both decode to `None`, which is the same shape as every
        // operator-configured source, so the row behaves identically.
        //
        // When it *is* present the credential is STS-style and unusable
        // without it — `update_encrypted`/`insert_encrypted` seal it with the
        // same `EncryptionService` as `secret_key`, and every signer and
        // shell-out reads it back from there.
        //
        // This is the boundary where a wire payload first becomes a stored
        // credential, so an empty string is normalised to `None` here rather
        // than left for each downstream signer to guess about: `Some("")` is
        // not "a session token", and one that reaches a SigV4 signer gets
        // signed as `X-Amz-Security-Token: ` and rejected by the provider.
        session_token: capability.session_token.filter(|token| !token.is_empty()),
        credentials_expire_at: capability.expires_at,
        region,
        endpoint: capability.endpoint,
        // Cloud-issued endpoints are always virtual-hosted-style S3 APIs;
        // path-style is the OSS-only MinIO/dev-loopback convention.
        force_path_style: Some(false),
    })
}

fn parse_backend(value: &str, allow_loopback_development: bool) -> Result<BackendUrl, CloudError> {
    if allow_loopback_development {
        BackendUrl::loopback_development(value)
    } else {
        BackendUrl::production(value)
    }
}

fn ready_managed_backup_setup() -> ManagedBackupSetup {
    ManagedBackupSetup {
        status: ManagedBackupSetupStatus::Ready,
        ready: true,
        message: "Managed backup destination is ready.".to_string(),
        action: ManagedBackupSetupAction::None,
    }
}

fn default_managed_backup_setup(backups_enabled: bool) -> ManagedBackupSetup {
    if backups_enabled {
        ManagedBackupSetup {
            status: ManagedBackupSetupStatus::NeedsSetup,
            ready: false,
            message: "Temps Cloud has not created the managed backup destination yet.".to_string(),
            action: ManagedBackupSetupAction::Retry,
        }
    } else {
        ManagedBackupSetup {
            status: ManagedBackupSetupStatus::Disabled,
            ready: false,
            message: "Enable Cloud backup export to provision the managed destination.".to_string(),
            action: ManagedBackupSetupAction::None,
        }
    }
}

fn managed_backup_setup_from_outcome(outcome: &ManagedBackupOutcome) -> ManagedBackupSetup {
    match outcome {
        ManagedBackupOutcome::Provisioned
        | ManagedBackupOutcome::ProvisionedBucketChanged { .. } => ready_managed_backup_setup(),
        ManagedBackupOutcome::NotConfigured { reason }
            if reason.as_deref().is_some_and(requires_subscription_renewal) =>
        {
            subscription_required_managed_backup_setup()
        }
        // Cloud's `reason` is operator-facing by design (the capability
        // contract requires a specific, actionable string whenever
        // `configured: false`) — surface it verbatim instead of a generic
        // placeholder, so an operator sees exactly why (no storage
        // configured, no entitlement, instance too old, ...) rather than a
        // single message covering every case identically.
        ManagedBackupOutcome::NotConfigured { reason } => ManagedBackupSetup {
            status: ManagedBackupSetupStatus::NeedsSetup,
            ready: false,
            message: reason.clone().unwrap_or_else(|| {
                "Temps Cloud has not created the managed backup destination yet.".to_string()
            }),
            action: ManagedBackupSetupAction::Retry,
        },
        ManagedBackupOutcome::Unavailable(reason) if requires_subscription_renewal(reason) => {
            subscription_required_managed_backup_setup()
        }
        // `reason` here is `error.to_string()` from the client call or the
        // local persistence step (see `provision_managed_backup_source`), not
        // raw transport internals, so it's safe and useful to show directly
        // rather than flattening every failure into one generic sentence.
        ManagedBackupOutcome::Unavailable(reason) => ManagedBackupSetup {
            status: ManagedBackupSetupStatus::Unavailable,
            ready: false,
            message: format!(
                "Temps Cloud could not create the managed backup destination: {reason}"
            ),
            action: ManagedBackupSetupAction::Retry,
        },
    }
}

fn subscription_required_managed_backup_setup() -> ManagedBackupSetup {
    ManagedBackupSetup {
        status: ManagedBackupSetupStatus::SubscriptionRequired,
        ready: false,
        message: "Your Temps Cloud subscription is inactive. Renew it before managed backups can be provisioned."
            .to_string(),
        action: ManagedBackupSetupAction::RenewSubscription,
    }
}

fn requires_subscription_renewal(reason: &str) -> bool {
    let normalized = reason.to_ascii_lowercase();
    [
        "subscription",
        "billing",
        "payment",
        "trial",
        "past_due",
        "past due",
        "expired",
        "canceled",
        "cancelled",
        "inactive",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

fn status_name(status: &temps_cloud_client::LinkStatus) -> &'static str {
    match status {
        temps_cloud_client::LinkStatus::StateUnreadable { .. } => "state_unreadable",
        temps_cloud_client::LinkStatus::NotConfigured => "not_configured",
        temps_cloud_client::LinkStatus::AwaitingEnrollment { .. } => "awaiting_enrollment",
        temps_cloud_client::LinkStatus::Linked { .. } => "linked",
        temps_cloud_client::LinkStatus::CredentialRejected { .. } => "credential_rejected",
    }
}

fn health_name(health: &temps_cloud_client::MirrorHealth) -> &'static str {
    match health {
        temps_cloud_client::MirrorHealth::Healthy => "healthy",
        temps_cloud_client::MirrorHealth::Buffering { .. } => "buffering",
        temps_cloud_client::MirrorHealth::Dropping { .. } => "dropping",
        temps_cloud_client::MirrorHealth::Degraded { .. } => "degraded",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inactive_subscription_requires_renewal_instead_of_retry() {
        let setup = managed_backup_setup_from_outcome(&ManagedBackupOutcome::NotConfigured {
            reason: Some("subscription expired".to_string()),
        });

        assert_eq!(setup.status, ManagedBackupSetupStatus::SubscriptionRequired);
        assert_eq!(setup.action, ManagedBackupSetupAction::RenewSubscription);
        assert!(!setup.ready);
    }

    #[test]
    fn transient_managed_backup_failure_can_be_retried() {
        let setup = managed_backup_setup_from_outcome(&ManagedBackupOutcome::Unavailable(
            "503 Service Unavailable".to_string(),
        ));

        assert_eq!(setup.status, ManagedBackupSetupStatus::Unavailable);
        assert_eq!(setup.action, ManagedBackupSetupAction::Retry);
        assert!(!setup.ready);
    }

    #[test]
    fn disabled_backup_export_has_no_setup_action() {
        let setup = default_managed_backup_setup(false);

        assert_eq!(setup.status, ManagedBackupSetupStatus::Disabled);
        assert_eq!(setup.action, ManagedBackupSetupAction::None);
        assert!(!setup.ready);
    }

    #[test]
    fn production_cloud_configuration_rejects_plain_http() {
        assert!(parse_backend("http://cloud.example.com", false).is_err());
        assert!(parse_backend("http://cloud.example.com", true).is_err());
    }

    #[test]
    fn loopback_http_requires_the_explicit_development_gate() {
        assert!(parse_backend("http://127.0.0.1:19200", false).is_err());
        assert!(parse_backend("http://127.0.0.1:19200", true).is_ok());
    }

    #[test]
    fn status_names_are_stable_api_values() {
        assert_eq!(
            status_name(&temps_cloud_client::LinkStatus::StateUnreadable {
                state_path: "/data/cloud-link/state.json".to_string(),
            }),
            "state_unreadable"
        );
        assert_eq!(
            status_name(&temps_cloud_client::LinkStatus::Linked {
                base_url: "https://cloud.test".to_string(),
            }),
            "linked"
        );
        assert_eq!(
            health_name(&temps_cloud_client::MirrorHealth::Buffering {
                spooled: 1,
                reason: "offline".to_string(),
            }),
            "buffering"
        );
    }

    #[tokio::test]
    async fn shutdown_aborts_in_flight_work_after_the_deadline() {
        let task = tokio::spawn(std::future::pending::<()>());
        tokio::time::timeout(
            Duration::from_secs(1),
            await_task_shutdown(task, "test mirror", Duration::from_millis(10)),
        )
        .await
        .unwrap();
    }

    fn capability(
        session_token: Option<&str>,
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> ManagedBackupCapability {
        ManagedBackupCapability {
            configured: true,
            endpoint: Some("https://example.r2.cloudflarestorage.com".to_string()),
            region: Some("auto".to_string()),
            bucket_name: Some("temps-cloud-shared".to_string()),
            bucket_path: Some("tenants/1/instances/2/managed-backups/".to_string()),
            access_key_id: Some("AKIA-vended".to_string()),
            secret_key: Some("vended-secret".to_string()),
            session_token: session_token.map(str::to_string),
            expires_at,
            reason: None,
        }
    }

    /// The write-back half of the credential-vending contract: whatever the
    /// rotation loop receives has to reach the `s3_sources` row, or the
    /// instance stores a temporary credential it cannot sign with.
    #[test]
    fn a_vended_temporary_credential_carries_its_token_and_expiry_into_storage() {
        let expires_at = chrono::DateTime::parse_from_rfc3339("2026-09-03T12:00:00Z")
            .expect("valid timestamp")
            .with_timezone(&chrono::Utc);

        let credentials = managed_backup_credentials_from_capability(capability(
            Some("vended-session-token"),
            Some(expires_at),
        ))
        .expect("a complete capability translates");

        assert_eq!(
            credentials.session_token.as_deref(),
            Some("vended-session-token")
        );
        assert_eq!(credentials.credentials_expire_at, Some(expires_at));
    }

    /// A backend that vends a long-lived credential — or one that predates the
    /// temporary-credential protocol and sends neither field — produces a row
    /// shaped exactly like an operator-configured source.
    #[test]
    fn a_vended_long_lived_credential_stores_no_session_token() {
        let credentials = managed_backup_credentials_from_capability(capability(None, None))
            .expect("a capability without the optional fields still translates");

        assert!(credentials.session_token.is_none());
        assert!(credentials.credentials_expire_at.is_none());
    }

    /// This is the boundary where a wire payload first becomes a stored
    /// credential, so `Some("")` — which only a backend can produce, never an
    /// existing self-hosted row — is normalised here rather than left for each
    /// downstream signer to guess about. An empty `X-Amz-Security-Token` gets
    /// signed and rejected; no token at all is correct.
    #[test]
    fn a_vended_empty_session_token_is_normalised_to_none() {
        let credentials = managed_backup_credentials_from_capability(capability(Some(""), None))
            .expect("an empty session token is not a translation failure");

        assert!(
            credentials.session_token.is_none(),
            "an empty session token must be indistinguishable from no session token"
        );
    }

    /// Neither secret may reach a log line or an error string through `{:?}`.
    #[test]
    fn translated_credentials_debug_output_redacts_both_secrets() {
        let credentials = managed_backup_credentials_from_capability(capability(
            Some("vended-session-token"),
            None,
        ))
        .expect("a complete capability translates");

        let rendered = format!("{credentials:?}");
        assert!(!rendered.contains("vended-secret"));
        assert!(!rendered.contains("vended-session-token"));
    }

    fn test_credentials(bucket_name: &str) -> temps_entities::s3_sources::S3SourceCredentials {
        temps_entities::s3_sources::S3SourceCredentials {
            name: MANAGED_BACKUP_SOURCE_NAME.to_string(),
            bucket_name: bucket_name.to_string(),
            bucket_path: "tenant".to_string(),
            access_key_id: "AKIA-rotated".to_string(),
            secret_key: "rotated-secret".to_string(),
            session_token: None,
            credentials_expire_at: None,
            region: "auto".to_string(),
            endpoint: Some("https://example.r2.cloudflarestorage.com".to_string()),
            force_path_style: Some(false),
        }
    }

    fn test_cloud_service(db: Arc<sea_orm::DatabaseConnection>) -> CloudService {
        let temp = tempfile::tempdir().expect("cloud-link state dir");
        let link = Arc::new(CloudLink::load_for_loopback_development(
            temp.path().to_path_buf(),
            "test-agent",
        ));
        // Never started, and never read by `upsert_managed_backup_source` —
        // just needed to satisfy the constructor.
        let config = Arc::new(ConfigService::new(
            Arc::new(
                temps_config::ServerConfig::new(
                    "127.0.0.1:3000".to_string(),
                    "postgresql://test".to_string(),
                    None,
                    Some("127.0.0.1:8000".to_string()),
                )
                .expect("ServerConfig::new"),
            ),
            db.clone(),
        ));
        let encryption = Arc::new(EncryptionService::new_from_password("cloud-service-test"));
        CloudService::new(link, config, db, encryption, true)
    }

    fn managed_row(id: i32, bucket_name: &str) -> temps_entities::s3_sources::Model {
        temps_entities::s3_sources::Model {
            id,
            name: MANAGED_BACKUP_SOURCE_NAME.to_string(),
            bucket_name: bucket_name.to_string(),
            bucket_path: "tenant".to_string(),
            access_key_id: "AKIA-old".to_string(),
            secret_key: "old-secret".to_string(),
            session_token: None,
            credentials_expire_at: None,
            region: "auto".to_string(),
            endpoint: Some("https://example.r2.cloudflarestorage.com".to_string()),
            force_path_style: Some(false),
            is_default: true,
            managed_by_cloud: true,
            lifecycle_reconcile_failed_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn upsert_rotates_in_place_when_the_bucket_is_unchanged() {
        let existing = managed_row(1, "temps-cloud-tenant-abc");
        let updated = managed_row(1, "temps-cloud-tenant-abc");
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_results(vec![vec![existing], vec![updated]])
                .into_connection(),
        );
        let service = test_cloud_service(db);

        let outcome = service
            .upsert_managed_backup_source(test_credentials("temps-cloud-tenant-abc"))
            .await
            .expect("rotation against the same bucket should succeed");

        assert!(matches!(outcome, UpsertOutcome::SameBucket));
    }

    #[tokio::test]
    async fn upsert_flags_a_bucket_change_instead_of_rotating_silently() {
        let existing = managed_row(1, "temps-cloud-tenant-abc");
        let updated = managed_row(1, "temps-cloud-tenant-XYZ-different");
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_results(vec![vec![existing], vec![updated]])
                .into_connection(),
        );
        let service = test_cloud_service(db);

        let outcome = service
            .upsert_managed_backup_source(test_credentials("temps-cloud-tenant-XYZ-different"))
            .await
            .expect("the credential should still be rotated even when the bucket changed");

        match outcome {
            UpsertOutcome::BucketChanged {
                previous_bucket_name,
                new_bucket_name,
            } => {
                assert_eq!(previous_bucket_name, "temps-cloud-tenant-abc");
                assert_eq!(new_bucket_name, "temps-cloud-tenant-XYZ-different");
            }
            UpsertOutcome::SameBucket => panic!("expected a bucket-changed outcome"),
        }
    }
}
