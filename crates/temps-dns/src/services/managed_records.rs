// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Ownership-guarded DNS record management (ADR-031)
//!
//! [`ManagedDnsRecordService`] is the ONLY path other crates should use to
//! create public A/AAAA/CNAME records in user zones. Unlike the raw
//! [`crate::services::DnsRecordService`] (which upserts blindly and is kept
//! for ACME challenge TXT records that temps unambiguously owns), every write
//! here is guarded by the ownership scheme from [`crate::ownership`]:
//!
//! - **Create/update** refuses if a record with the target name/type exists
//!   without a temps ownership marker, or with a marker from a different
//!   temps install — AND refuses if the ownership registry name itself is
//!   occupied by a TXT record that is not our marker (so the marker write can
//!   never clobber someone else's TXT). Conflicts surface as typed
//!   [`DnsError::RecordConflict`] / [`DnsError::NotOwnedByInstance`] so the
//!   UI can offer import-or-skip.
//! - **Delete** only removes records this install owns.
//! - **Import** is the explicit, user-confirmed adoption path that stamps a
//!   marker onto a pre-existing record.
//!
//! Proxied (Cloudflare orange-cloud) writes additionally pass the proxy
//! capability + Universal SSL depth gate — see
//! [`crate::ownership::check_proxy_allowed`].
//!
//! # Crash ordering
//!
//! For a fresh record, a marker bound to the intended record value is written
//! BEFORE the target. A crash between the two leaves a signed orphan marker
//! that can only authorize recreating that exact value (or be cleaned up).
//! Updates keep the marker bound to the old value until the target write
//! succeeds, then commit the replacement marker.
//!
//! # Concurrency (TOCTOU)
//!
//! DNS provider APIs have no compare-and-swap, so a check-then-write window
//! against the remote zone is unavoidable. Fresh records (marker and target)
//! are therefore written with the provider's create-only call, never an
//! upsert: a record created by someone else between our ownership check and
//! our write makes the create fail (or, on providers that allow multiple
//! values, adds a sibling that the next guarded read reports as a conflict)
//! instead of being overwritten. Updates still replace the exact record whose
//! signed marker we verified. Guarded operations on the same (zone, record
//! name) are serialized within a process by a keyed async lock and across
//! processes sharing one Temps database by a PostgreSQL advisory transaction
//! lock; contention on that lock surfaces as [`DnsError::RecordLocked`]
//! (retryable), never as an ownership conflict.
//!
//! # Scope
//!
//! Every marker carries the signed (controller, project, environment) of the
//! workflow that wrote it. The guarded core refuses to update, delete, re-use
//! or re-stamp a marker whose scope the caller does not match (see
//! [`OwnershipScope::permits`]), so the generic records API cannot overwrite
//! or delete a record owned by domain delivery or generated-hostname sync.
//!
//! # Providers
//!
//! Only providers whose writes touch exactly one (name, type) are accepted
//! ([`crate::providers::DnsProvider::lossless_per_record_writes`]); a
//! whole-zone writer such as Namecheap would turn each guarded write into a
//! rewrite of every unrelated record.
//!
//! # Removal granularity
//!
//! Ownership is per exact provider record. Multiple values at the same
//! (name, type) are treated as a conflict, and deletion uses the provider's
//! exact record identifier so unrelated values are never removed.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ConnectionTrait, DatabaseBackend, DatabaseConnection,
    DatabaseTransaction, EntityTrait, Statement, TransactionTrait,
};
use temps_entities::{dns_instance_identity, environments, projects};
use tracing::{info, warn};

use crate::errors::{DnsError, MarkerNotFinalized, OwnershipScopeConflict};
use crate::ownership::{
    check_proxy_allowed, record_fingerprint, registry_record_name, OwnershipMarker,
    OWNERSHIP_REGISTRY_PREFIX,
};
use crate::providers::{DnsProvider, DnsRecord, DnsRecordContent, DnsRecordRequest, DnsRecordType};

/// Record types that route traffic for a name. Owning one of them never
/// grants the right to add another next to a record someone else controls.
const ROUTING_RECORD_TYPES: [DnsRecordType; 3] =
    [DnsRecordType::A, DnsRecordType::AAAA, DnsRecordType::CNAME];
use crate::services::provider_service::DnsProviderService;

/// What a managed record was created for; stamped into the ownership marker
/// so the provider-side registry shows which project/environment a record
/// belongs to.
#[derive(Debug, Clone, Copy, Default)]
pub struct OwnershipScope {
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
    pub controller: Option<&'static str>,
}

impl OwnershipScope {
    /// Scope of an automation controller acting on all of its own records,
    /// regardless of project/environment (e.g. removing a record whose
    /// binding the controller has already resolved).
    pub const fn for_controller(controller: &'static str) -> Self {
        Self {
            project_id: None,
            environment_id: None,
            controller: Some(controller),
        }
    }

    /// Whether a caller acting with this scope may modify a record whose
    /// signed marker was written under `marker`'s scope.
    ///
    /// - The controller must match exactly: `None` (the generic records API)
    ///   never matches `Some("domain-delivery")`, and two controllers never
    ///   match each other. This is what stops one workflow from overwriting
    ///   or deleting records another workflow is still bound to.
    /// - A project/environment the caller specifies must equal the marker's.
    ///   A caller that does not specify one (`None`) is not acting for a
    ///   particular project/environment and is not constrained on it.
    pub fn permits(&self, marker: &OwnershipMarker) -> bool {
        marker.controller.as_deref() == self.controller
            && self
                .project_id
                .is_none_or(|project_id| marker.project_id == Some(project_id))
            && self
                .environment_id
                .is_none_or(|environment_id| marker.environment_id == Some(environment_id))
    }

    /// Scope to stamp into a replacement marker: the caller's scope, keeping
    /// the existing marker's project/environment where the caller did not
    /// specify one so an unscoped update never silently drops those labels.
    fn stamped_over(self, existing: Option<&OwnershipMarker>) -> Self {
        let Some(existing) = existing else {
            return self;
        };
        Self {
            project_id: self.project_id.or(existing.project_id),
            environment_id: self.environment_id.or(existing.environment_id),
            controller: self.controller,
        }
    }

    /// Typed refusal for a marker this scope does not [`permit`](Self::permits).
    fn refusal(
        &self,
        zone: &str,
        name: &str,
        record_type: DnsRecordType,
        marker: &OwnershipMarker,
    ) -> DnsError {
        DnsError::OwnedByOtherScope(Box::new(OwnershipScopeConflict {
            zone: zone.to_string(),
            name: name.to_string(),
            record_type: record_type.to_string(),
            owner_controller: marker.controller.clone(),
            owner_project_id: marker.project_id,
            owner_environment_id: marker.environment_id,
            requester_controller: self.controller.map(str::to_string),
            requester_project_id: self.project_id,
            requester_environment_id: self.environment_id,
        }))
    }
}

/// Ownership state of a record at the provider, for the domain UI's
/// per-record status (created / conflict / unmanaged).
#[derive(Debug, Clone)]
pub enum RecordOwnership {
    /// No record with this name/type exists.
    NotFound,
    /// Record exists but carries no temps ownership marker — temps will not
    /// touch it unless the user imports it.
    Unmanaged(DnsRecord),
    /// Record exists and is owned by this temps install.
    Owned(DnsRecord, OwnershipMarker),
    /// Record exists and is owned by a DIFFERENT temps install.
    OwnedByOther(DnsRecord, OwnershipMarker),
    /// This install's signed marker exists, but its target record is absent.
    Orphaned(OwnershipMarker),
    /// A different temps install's marker exists without a target record.
    BlockedByOther(OwnershipMarker),
    /// The reserved ownership registry name contains foreign or ambiguous TXT.
    RegistryConflict,
}

/// State of the ownership registry name itself (the `_temps-owned-<type>.…`
/// TXT), independent of whether the target record exists. Distinguishing
/// "no TXT" from "a TXT that is not our marker" is what keeps the marker
/// write from ever clobbering foreign content.
#[derive(Debug, Clone)]
enum RegistryState {
    /// No TXT record at the registry name.
    Absent,
    /// Our marker (this instance, covering this record type).
    Owned(OwnershipMarker, Box<DnsRecord>),
    /// A valid temps marker from a different install.
    Foreign(OwnershipMarker),
    /// A TXT record exists but is not a marker that covers this
    /// (instance, type) — user content or a tampered/mismatched marker.
    /// Never overwrite it.
    Occupied,
}

/// Per-key async locks that self-clean when the last holder releases.
///
/// Keys are unbounded user input (zone + record name), so entries are removed
/// as soon as no task holds or waits on them — memory stays proportional to
/// in-flight operations, not to history (CLAUDE.md bounded-memory rule).
type LockMap = HashMap<(String, String), Arc<tokio::sync::Mutex<()>>>;

struct KeyedLocks {
    inner: std::sync::Mutex<LockMap>,
}

impl KeyedLocks {
    fn new() -> Self {
        Self {
            inner: std::sync::Mutex::new(HashMap::new()),
        }
    }

    async fn acquire(self: &Arc<Self>, zone: &str, name: &str) -> KeyedLockLease {
        // Keys are normalized here, not by callers, so `App.` in `Example.COM`
        // and `app` in `example.com` always serialize on the same lock.
        // Poison-proof: the critical section is a plain HashMap op that can't
        // panic, but if it somehow did, recovering the map beats turning every
        // future DNS write into a panic until restart.
        let key = (
            normalize_lock_component(zone),
            normalize_lock_component(name),
        );
        let handle = {
            let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            map.entry(key.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let guard = handle.clone().lock_owned().await;
        KeyedLockLease {
            owner: self.clone(),
            key,
            handle,
            guard: Some(guard),
        }
    }
}

pub(crate) struct KeyedLockLease {
    owner: Arc<KeyedLocks>,
    key: (String, String),
    handle: Arc<tokio::sync::Mutex<()>>,
    guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl Drop for KeyedLockLease {
    fn drop(&mut self) {
        self.guard.take();
        let mut map = self.owner.inner.lock().unwrap_or_else(|e| e.into_inner());
        if Arc::strong_count(&self.handle) == 2
            && map
                .get(&self.key)
                .is_some_and(|current| Arc::ptr_eq(current, &self.handle))
        {
            map.remove(&self.key);
        }
    }
}

/// Canonical form of a lock key component: trimmed, lowercase, no root dot.
fn normalize_lock_component(value: &str) -> String {
    value.trim().trim_end_matches('.').to_ascii_lowercase()
}

fn shared_keyed_locks() -> Arc<KeyedLocks> {
    static LOCKS: OnceLock<Arc<KeyedLocks>> = OnceLock::new();
    LOCKS.get_or_init(|| Arc::new(KeyedLocks::new())).clone()
}

/// Ownership-guarded record management on top of [`DnsProviderService`].
pub struct ManagedDnsRecordService {
    db: Arc<DatabaseConnection>,
    provider_service: Arc<DnsProviderService>,
    signing_key: [u8; 32],
    instance_id: tokio::sync::OnceCell<String>,
    locks: Arc<KeyedLocks>,
}

impl ManagedDnsRecordService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        provider_service: Arc<DnsProviderService>,
        encryption_service: Arc<temps_core::EncryptionService>,
    ) -> Self {
        Self {
            db,
            provider_service,
            signing_key: encryption_service.derive_subkey("temps:dns-ownership:v1"),
            instance_id: tokio::sync::OnceCell::new(),
            locks: shared_keyed_locks(),
        }
    }

    /// Get (or create on first use) this install's ownership instance ID.
    ///
    /// The ID never rotates once created — rotating would orphan every record
    /// this install previously stamped.
    pub async fn instance_id(&self) -> Result<String, DnsError> {
        let id = self
            .instance_id
            .get_or_try_init(|| Self::load_instance_id(self.db.as_ref()))
            .await?;
        Ok(id.clone())
    }

    pub(crate) async fn load_instance_id(db: &DatabaseConnection) -> Result<String, DnsError> {
        if let Some(row) = dns_instance_identity::Entity::find().one(db).await? {
            return Ok(row.instance_id);
        }

        let fresh = uuid::Uuid::new_v4().to_string();
        let row = dns_instance_identity::ActiveModel {
            id: Set(1),
            instance_id: Set(fresh),
            ..Default::default()
        };
        // Two concurrent first writes can race on the single-row PK; whoever
        // loses re-reads the winner's ID instead of failing.
        match row.insert(db).await {
            Ok(created) => Ok(created.instance_id),
            Err(insert_error) => dns_instance_identity::Entity::find()
                .one(db)
                .await?
                .map(|row| row.instance_id)
                .ok_or(DnsError::Database(insert_error)),
        }
    }

    /// Serialize every in-process ownership read/modify/write sequence for an
    /// exact provider record, including generated-hostname reconciliation.
    pub(crate) async fn lock_record(zone: &str, name: &str) -> KeyedLockLease {
        shared_keyed_locks().acquire(zone, name).await
    }

    /// Serialize writers on every process sharing this installation's database.
    /// The transaction stays open across the provider read/write sequence; its
    /// advisory lock is released when the transaction is dropped.
    pub(crate) async fn lock_record_in_db(
        db: &DatabaseConnection,
        zone: &str,
        name: &str,
    ) -> Result<DatabaseTransaction, DnsError> {
        let transaction = db.begin().await?;
        Self::lock_record_on_transaction(&transaction, zone, name).await?;
        Ok(transaction)
    }

    pub(crate) async fn lock_record_on_transaction(
        transaction: &DatabaseTransaction,
        zone: &str,
        name: &str,
    ) -> Result<(), DnsError> {
        let key = format!(
            "managed-dns:{}:{}",
            normalize_lock_component(zone),
            normalize_lock_component(name)
        );
        let row = transaction
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT pg_try_advisory_xact_lock(hashtext($1)) AS acquired",
                [key.into()],
            ))
            .await?
            .ok_or_else(|| {
                DnsError::Database(sea_orm::DbErr::Custom(
                    "PostgreSQL DNS advisory lock query returned no row".into(),
                ))
            })?;
        let acquired: bool = row.try_get("", "acquired")?;
        if !acquired {
            // Contention is transient and retryable — a distinct variant so
            // callers never confuse it with a real ownership conflict.
            return Err(DnsError::RecordLocked {
                zone: zone.into(),
                name: name.into(),
            });
        }
        Ok(())
    }

    /// Create or update a managed record, enforcing ownership and proxy
    /// guardrails. `domain` may be any FQDN under a managed zone; the record
    /// `request.name` is relative to that zone.
    ///
    /// If the managed domain has `proxied_by_default` set, the record is
    /// proxied even when the request doesn't ask for it (a per-record
    /// `proxied: true` also always wins).
    pub async fn set_managed_record(
        &self,
        domain: &str,
        request: DnsRecordRequest,
        proxied_override: Option<bool>,
        scope: OwnershipScope,
    ) -> Result<DnsRecord, DnsError> {
        self.set_managed_record_with_transaction(domain, request, proxied_override, scope, None)
            .await
    }

    pub(crate) async fn set_managed_record_with_transaction(
        &self,
        domain: &str,
        mut request: DnsRecordRequest,
        proxied_override: Option<bool>,
        scope: OwnershipScope,
        transaction: Option<&DatabaseTransaction>,
    ) -> Result<DnsRecord, DnsError> {
        self.validate_scope(scope).await?;
        let (provider_model, managed) = self
            .provider_service
            .find_provider_for_domain(domain)
            .await?
            .ok_or_else(|| DnsError::DomainNotManaged(domain.to_string()))?;
        let provider = self
            .provider_service
            .create_provider_instance(&provider_model)?;
        let zone = managed.domain.clone();

        request.name = Self::validate_record_request(&zone, &request)?;
        request.proxied = proxied_override.unwrap_or(managed.proxied_by_default);
        Self::validate_provider_capabilities(
            &provider.capabilities(),
            &provider_model.name,
            request.content.record_type(),
        )?;
        if request.proxied {
            check_proxy_allowed(
                &provider.capabilities(),
                &provider_model.name,
                &zone,
                &request.name,
            )?;
        }

        let instance = self.instance_id().await?;
        let name = request.name.clone();
        let _db_lock = if let Some(transaction) = transaction {
            Self::lock_record_on_transaction(transaction, &zone, &name).await?;
            None
        } else {
            Some(Self::lock_record_in_db(self.db.as_ref(), &zone, &name).await?)
        };
        let _lease = self.locks.acquire(&zone, &name).await;
        let record = Self::guarded_set(
            provider.as_ref(),
            &zone,
            request,
            &instance,
            &self.signing_key,
            scope,
        )
        .await?;

        info!(
            "Set managed {} record '{}' in zone {} via provider {} (proxied: {})",
            record.content.record_type(),
            record.name,
            zone,
            provider_model.name,
            record.proxied
        );
        Ok(record)
    }

    /// Delete a managed record. Refuses unless this install owns it AND the
    /// record's marker was written under a scope `scope` permits (see
    /// [`OwnershipScope::permits`]).
    pub async fn remove_managed_record(
        &self,
        domain: &str,
        name: &str,
        record_type: DnsRecordType,
        scope: OwnershipScope,
    ) -> Result<(), DnsError> {
        self.remove_managed_record_with_transaction(domain, name, record_type, scope, None)
            .await
    }

    pub(crate) async fn remove_managed_record_with_transaction(
        &self,
        domain: &str,
        name: &str,
        record_type: DnsRecordType,
        scope: OwnershipScope,
        transaction: Option<&DatabaseTransaction>,
    ) -> Result<(), DnsError> {
        let (provider_model, managed) = self
            .provider_service
            .find_provider_for_domain(domain)
            .await?
            .ok_or_else(|| DnsError::DomainNotManaged(domain.to_string()))?;
        let provider = self
            .provider_service
            .create_provider_instance(&provider_model)?;
        let zone = managed.domain.clone();
        Self::validate_record_type(record_type)?;
        let name = Self::validate_record_name(&zone, name)?;
        Self::validate_provider_capabilities(
            &provider.capabilities(),
            &provider_model.name,
            record_type,
        )?;

        let instance = self.instance_id().await?;
        let _db_lock = if let Some(transaction) = transaction {
            Self::lock_record_on_transaction(transaction, &zone, &name).await?;
            None
        } else {
            Some(Self::lock_record_in_db(self.db.as_ref(), &zone, &name).await?)
        };
        let _lease = self.locks.acquire(&zone, &name).await;
        Self::guarded_remove(
            provider.as_ref(),
            &zone,
            &name,
            record_type,
            &instance,
            &self.signing_key,
            scope,
        )
        .await?;

        info!(
            "Removed managed {} record '{}' in zone {} via provider {}",
            record_type, name, zone, provider_model.name
        );
        Ok(())
    }

    /// Explicitly adopt a pre-existing record into temps management by
    /// stamping an ownership marker onto it. This is the user-confirmed
    /// "import" arm of the conflict flow — never called automatically.
    pub async fn import_record(
        &self,
        domain: &str,
        name: &str,
        record_type: DnsRecordType,
        scope: OwnershipScope,
    ) -> Result<OwnershipMarker, DnsError> {
        self.import_record_with_transaction(domain, name, record_type, scope, None)
            .await
    }

    pub(crate) async fn import_record_with_transaction(
        &self,
        domain: &str,
        name: &str,
        record_type: DnsRecordType,
        scope: OwnershipScope,
        transaction: Option<&DatabaseTransaction>,
    ) -> Result<OwnershipMarker, DnsError> {
        self.validate_scope(scope).await?;
        let (provider_model, managed) = self
            .provider_service
            .find_provider_for_domain(domain)
            .await?
            .ok_or_else(|| DnsError::DomainNotManaged(domain.to_string()))?;
        let provider = self
            .provider_service
            .create_provider_instance(&provider_model)?;
        let zone = managed.domain.clone();
        Self::validate_record_type(record_type)?;
        let name = Self::validate_record_name(&zone, name)?;
        Self::validate_provider_capabilities(
            &provider.capabilities(),
            &provider_model.name,
            record_type,
        )?;

        let instance = self.instance_id().await?;
        let _db_lock = if let Some(transaction) = transaction {
            Self::lock_record_on_transaction(transaction, &zone, &name).await?;
            None
        } else {
            Some(Self::lock_record_in_db(self.db.as_ref(), &zone, &name).await?)
        };
        let _lease = self.locks.acquire(&zone, &name).await;
        let marker = Self::guarded_import(
            provider.as_ref(),
            &zone,
            &name,
            record_type,
            None,
            &instance,
            &self.signing_key,
            scope,
        )
        .await?;

        info!(
            "Imported {} record '{}' in zone {} into temps management",
            record_type, name, zone
        );
        Ok(marker)
    }

    /// Ownership state of a record, for the domain UI.
    pub async fn record_ownership(
        &self,
        domain: &str,
        name: &str,
        record_type: DnsRecordType,
    ) -> Result<RecordOwnership, DnsError> {
        let (provider_model, managed) = self
            .provider_service
            .find_provider_for_domain(domain)
            .await?
            .ok_or_else(|| DnsError::DomainNotManaged(domain.to_string()))?;
        let provider = self
            .provider_service
            .create_provider_instance(&provider_model)?;
        Self::validate_record_type(record_type)?;
        let name = Self::validate_record_name(&managed.domain, name)?;
        Self::validate_provider_capabilities(
            &provider.capabilities(),
            &provider_model.name,
            record_type,
        )?;
        let instance = self.instance_id().await?;

        Self::ownership_of(
            provider.as_ref(),
            &managed.domain,
            &name,
            record_type,
            &instance,
            &self.signing_key,
        )
        .await
    }

    async fn validate_scope(&self, scope: OwnershipScope) -> Result<(), DnsError> {
        let project = if let Some(project_id) = scope.project_id {
            Some(
                projects::Entity::find_by_id(project_id)
                    .one(self.db.as_ref())
                    .await?
                    .filter(|project| !project.is_deleted)
                    .ok_or_else(|| {
                        DnsError::Validation(format!(
                            "Cannot assign DNS ownership to missing project {project_id}"
                        ))
                    })?,
            )
        } else {
            None
        };

        if let Some(environment_id) = scope.environment_id {
            let environment = environments::Entity::find_by_id(environment_id)
                .one(self.db.as_ref())
                .await?
                .filter(|environment| environment.deleted_at.is_none())
                .ok_or_else(|| {
                    DnsError::Validation(format!(
                        "Cannot assign DNS ownership to missing environment {environment_id}"
                    ))
                })?;
            if let Some(project) = project {
                if environment.project_id != project.id {
                    return Err(DnsError::Validation(format!(
                        "Environment {environment_id} belongs to project {}, not project {}",
                        environment.project_id, project.id
                    )));
                }
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Guarded core — associated functions over `&dyn DnsProvider` so the
    // safety logic is unit-testable with an in-memory provider, independent
    // of the database and real provider APIs.
    // ------------------------------------------------------------------

    /// State of the ownership registry TXT for (name, type).
    async fn registry_state(
        provider: &dyn DnsProvider,
        zone: &str,
        record_name: &str,
        record_type: DnsRecordType,
        instance: &str,
        signing_key: &[u8; 32],
    ) -> Result<RegistryState, DnsError> {
        let registry_name = registry_record_name(record_name, record_type);
        let mut records = provider
            .get_records(zone, &registry_name, DnsRecordType::TXT)
            .await?;
        if records.is_empty() {
            return Ok(RegistryState::Absent);
        }
        if records.len() != 1 {
            return Ok(RegistryState::Occupied);
        }
        let record = records.remove(0);
        let DnsRecordContent::TXT { content } = &record.content else {
            return Ok(RegistryState::Occupied);
        };
        let marker = OwnershipMarker::parse_at(content, zone, record_name);
        Ok(match marker {
            None => RegistryState::Occupied,
            Some(marker) if !marker.is_owned_by(instance) => RegistryState::Foreign(marker),
            Some(marker)
                if marker.covers(signing_key, instance, zone, record_name, record_type) =>
            {
                RegistryState::Owned(marker, Box::new(record))
            }
            Some(_) => RegistryState::Occupied,
        })
    }

    pub(crate) async fn ownership_of(
        provider: &dyn DnsProvider,
        zone: &str,
        name: &str,
        record_type: DnsRecordType,
        instance: &str,
        signing_key: &[u8; 32],
    ) -> Result<RecordOwnership, DnsError> {
        let mut existing = provider.get_records(zone, name, record_type).await?;
        if existing.is_empty() {
            return match Self::registry_state(
                provider,
                zone,
                name,
                record_type,
                instance,
                signing_key,
            )
            .await?
            {
                RegistryState::Absent => Ok(RecordOwnership::NotFound),
                RegistryState::Owned(marker, _) => Ok(RecordOwnership::Orphaned(marker)),
                RegistryState::Foreign(marker) => Ok(RecordOwnership::BlockedByOther(marker)),
                RegistryState::Occupied => Ok(RecordOwnership::RegistryConflict),
            };
        }
        if existing.len() != 1 {
            return Ok(RecordOwnership::Unmanaged(existing.remove(0)));
        }
        let record = existing.remove(0);
        match Self::registry_state(provider, zone, name, record_type, instance, signing_key).await?
        {
            RegistryState::Owned(marker, _)
                if marker
                    .matches_fingerprint(&record_fingerprint(&record.content, record.proxied)?) =>
            {
                Ok(RecordOwnership::Owned(record, marker))
            }
            RegistryState::Owned(_, _) => Ok(RecordOwnership::Unmanaged(record)),
            RegistryState::Foreign(marker) => Ok(RecordOwnership::OwnedByOther(record, marker)),
            RegistryState::Absent | RegistryState::Occupied => {
                Ok(RecordOwnership::Unmanaged(record))
            }
        }
    }

    /// Refuse providers whose writes are not scoped to a single (name, type).
    ///
    /// Checked inside the guarded core (not only at the service entry) so
    /// every caller — API, domain delivery, generated-hostname sync — gets the
    /// same refusal.
    fn require_lossless_writes(
        provider: &dyn DnsProvider,
        zone: &str,
        name: &str,
        record_type: DnsRecordType,
    ) -> Result<(), DnsError> {
        if provider.lossless_per_record_writes() {
            return Ok(());
        }
        Err(DnsError::NotSupported(format!(
            "DNS provider type '{}' cannot be used for ownership-guarded management of {record_type} '{name}' in zone {zone}: its API can only rewrite the whole zone, so every write would also rewrite unrelated records and could drop ones it cannot represent (URL redirects, ALIAS, CAA, mail settings). Manage this record at the provider directly, or move the zone to a DNS provider with per-record writes",
            provider.provider_type()
        )))
    }

    /// Before creating a NEW routing record, make sure no other routing type
    /// at the same name belongs to someone else. Ownership is per (name,
    /// type), so without this check temps could add `app` A next to a user's
    /// `app` AAAA (or CNAME) and split the name's traffic between them.
    #[allow(clippy::too_many_arguments)]
    async fn ensure_no_foreign_routing_siblings(
        provider: &dyn DnsProvider,
        zone: &str,
        name: &str,
        record_type: DnsRecordType,
        instance: &str,
        signing_key: &[u8; 32],
        scope: OwnershipScope,
    ) -> Result<(), DnsError> {
        for sibling_type in ROUTING_RECORD_TYPES {
            if sibling_type == record_type {
                continue;
            }
            let ownership =
                Self::ownership_of(provider, zone, name, sibling_type, instance, signing_key)
                    .await?;
            let reason = match ownership {
                // No sibling record exists (markers alone route nothing).
                RecordOwnership::NotFound
                | RecordOwnership::Orphaned(_)
                | RecordOwnership::BlockedByOther(_)
                | RecordOwnership::RegistryConflict => continue,
                RecordOwnership::Owned(_, marker) if scope.permits(&marker) => continue,
                RecordOwnership::Owned(_, _) => format!(
                    "a {sibling_type} record already exists at this name and is managed by temps for a different workflow or project; adding a {record_type} record next to it would split the name's traffic"
                ),
                RecordOwnership::OwnedByOther(_, _) => format!(
                    "a {sibling_type} record already exists at this name and is managed by another temps install; adding a {record_type} record next to it would split the name's traffic"
                ),
                RecordOwnership::Unmanaged(_) => format!(
                    "a {sibling_type} record already exists at this name and is not managed by temps; adding a {record_type} record next to it would split the name's traffic"
                ),
            };
            return Err(Self::record_conflict(zone, name, record_type, &reason));
        }
        Ok(())
    }

    /// Write a marker request onto an existing registry record, by its exact
    /// provider ID when the provider returned one.
    async fn rewrite_marker(
        provider: &dyn DnsProvider,
        zone: &str,
        marker_record: &DnsRecord,
        request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError> {
        match marker_record.id.as_deref() {
            Some(id) => provider.update_record(zone, id, request).await,
            None => provider.set_record(zone, request).await,
        }
    }

    pub(crate) async fn guarded_set(
        provider: &dyn DnsProvider,
        zone: &str,
        mut request: DnsRecordRequest,
        instance: &str,
        signing_key: &[u8; 32],
        scope: OwnershipScope,
    ) -> Result<DnsRecord, DnsError> {
        // Every guarded write sends the canonical spelling (lowercase CNAME
        // target without a root dot, canonical IP text), whichever caller
        // built the request. Providers that echo a write verbatim then
        // return what they will later list, and the marker below is signed
        // over that same data.
        request.content = request.content.canonical();
        let record_type = request.content.record_type();
        Self::require_lossless_writes(provider, zone, &request.name, record_type)?;
        let mut existing = provider
            .get_records(zone, &request.name, record_type)
            .await?;
        if existing.len() > 1 {
            return Err(Self::record_conflict(
                zone,
                &request.name,
                record_type,
                "multiple provider records exist at this name and type; temps manages exactly one record per name and type, so remove the extra records at the provider, then retry",
            ));
        }
        let existing = existing.pop();
        let desired_fingerprint = record_fingerprint(&request.content, request.proxied)?;
        let current_fingerprint = existing
            .as_ref()
            .map(|record| record_fingerprint(&record.content, record.proxied))
            .transpose()?;
        let registry = Self::registry_state(
            provider,
            zone,
            &request.name,
            record_type,
            instance,
            signing_key,
        )
        .await?;

        match (&existing, &registry) {
            (_, RegistryState::Owned(marker, _)) if !scope.permits(marker) => {
                return Err(scope.refusal(zone, &request.name, record_type, marker));
            }
            (Some(_), RegistryState::Owned(marker, _)) => {
                let current = current_fingerprint.as_deref().unwrap_or_default();
                if !marker.matches_fingerprint(current) {
                    return Err(Self::record_conflict(
                        zone,
                        &request.name,
                        record_type,
                        "the ownership marker is stale and does not match the provider record; explicitly import the record to adopt its current value",
                    ));
                }
            }
            (None, RegistryState::Owned(marker, _))
                if marker.matches_fingerprint(&desired_fingerprint) => {}
            (None, RegistryState::Owned(_, _)) => {
                return Err(Self::record_conflict(
                    zone,
                    &request.name,
                    record_type,
                    "an orphan ownership marker exists for different record content; remove it before retrying",
                ));
            }
            (None, RegistryState::Absent) => {}
            (Some(_), RegistryState::Absent) => {
                return Err(Self::record_conflict(
                    zone,
                    &request.name,
                    record_type,
                    "an existing record with this name is not managed by temps, and temps never overwrites a record it does not manage; adopt the record explicitly or remove it at the provider, then retry",
                ));
            }
            (Some(_), RegistryState::Occupied) => {
                return Err(Self::record_conflict(
                    zone,
                    &request.name,
                    record_type,
                    &format!(
                        "an existing record with this name is not managed by temps, and a TXT record that is not a temps marker occupies its ownership registry name '{}'; remove that TXT record at the provider, then retry",
                        registry_record_name(&request.name, record_type)
                    ),
                ));
            }
            (None, RegistryState::Occupied) => {
                return Err(Self::record_conflict(zone, &request.name, record_type, &format!(
                        "a TXT record already occupies the ownership registry name '{}' and is not a temps marker",
                        registry_record_name(&request.name, record_type)
                    )));
            }
            (_, RegistryState::Foreign(marker)) => {
                return Err(DnsError::NotOwnedByInstance {
                    domain: zone.to_string(),
                    name: request.name.clone(),
                    record_type: record_type.to_string(),
                    owner_instance: marker.instance.clone(),
                });
            }
        }

        let fresh_create = existing.is_none();
        if fresh_create {
            Self::ensure_no_foreign_routing_siblings(
                provider,
                zone,
                &request.name,
                record_type,
                instance,
                signing_key,
                scope,
            )
            .await?;
        }

        let (existing_marker, existing_marker_record) = match &registry {
            RegistryState::Owned(marker, record) => (Some(marker), Some(record.as_ref())),
            RegistryState::Absent | RegistryState::Foreign(_) | RegistryState::Occupied => {
                (None, None)
            }
        };
        let stamp_scope = scope.stamped_over(existing_marker);
        let registry_name = registry_record_name(&request.name, record_type);
        let sign = |name: &str, fingerprint: &str| {
            OwnershipMarker::new_signed(
                signing_key,
                instance,
                zone,
                name,
                record_type,
                fingerprint,
                stamp_scope.project_id,
                stamp_scope.environment_id,
                stamp_scope.controller,
            )
        };

        // Creates are marker-first and create-only: a TXT or target record
        // that appeared after our checks makes the create fail instead of
        // being overwritten.
        //
        // Updates first re-sign the marker for the live value plus the value
        // being written (its pending fingerprint). The marker then covers the
        // record whichever step fails next: this marker write, the record
        // write (including one that reports an error after the provider
        // applied it), or the final marker write. It never covers a value
        // nobody requested, so content anyone else writes stays unmanaged.
        let prepared_marker = if fresh_create {
            let registry_request =
                Self::marker_request(&registry_name, &sign(&request.name, &desired_fingerprint)?)?;
            Some(match existing_marker_record {
                // Our own orphan marker for this exact value: rewrite it in
                // place (by ID) with the caller's scope.
                Some(orphan) => {
                    Self::rewrite_marker(provider, zone, orphan, registry_request).await?
                }
                None => provider.create_record(zone, registry_request).await?,
            })
        } else {
            match (current_fingerprint.as_deref(), existing_marker_record) {
                (Some(current), Some(marker_record)) if current != desired_fingerprint => {
                    let pending = sign(&request.name, current)?
                        .with_pending_fingerprint(signing_key, &desired_fingerprint)?;
                    Some(
                        Self::rewrite_marker(
                            provider,
                            zone,
                            marker_record,
                            Self::marker_request(&registry_name, &pending)?,
                        )
                        .await?,
                    )
                }
                _ => None,
            }
        };

        let record_name = request.name.clone();
        let target_result = if fresh_create {
            provider.create_record(zone, request).await
        } else {
            provider.set_record(zone, request).await
        };

        match target_result {
            Ok(record) => {
                let actual_fingerprint = record_fingerprint(&record.content, record.proxied)?;
                let committed_request = Self::marker_request(
                    &registry_name,
                    &sign(&record.name, &actual_fingerprint)?,
                )?;
                let finalized = match prepared_marker.as_ref().or(existing_marker_record) {
                    Some(marker_record) => {
                        Self::rewrite_marker(provider, zone, marker_record, committed_request).await
                    }
                    None => provider.set_record(zone, committed_request).await,
                };
                if let Err(source) = finalized {
                    // The record already changed, so a bare provider error
                    // would read as "nothing happened". The marker in place
                    // covers the requested value, and on an update also the
                    // previous one; anything else the provider stored is not
                    // covered.
                    let stays_managed = actual_fingerprint == desired_fingerprint
                        || current_fingerprint.as_deref() == Some(actual_fingerprint.as_str());
                    warn!(
                        "Wrote {} record '{}' in zone {} but could not finalize its ownership marker '{}' (stays managed: {}): {}",
                        record_type, record.name, zone, registry_name, stays_managed, source
                    );
                    return Err(DnsError::ManagedRecordMarkerNotFinalized(Box::new(
                        MarkerNotFinalized {
                            zone: zone.to_string(),
                            name: record.name.clone(),
                            record_type: record_type.to_string(),
                            stays_managed,
                            proxied: record.proxied,
                            source,
                        },
                    )));
                }
                Ok(record)
            }
            Err(e) => {
                if fresh_create {
                    if let RegistryState::Absent = registry {
                        Self::clean_up_fresh_marker(
                            provider,
                            zone,
                            &record_name,
                            record_type,
                            &desired_fingerprint,
                            &registry_name,
                            prepared_marker.as_ref(),
                        )
                        .await;
                    }
                }
                Err(e)
            }
        }
    }

    /// After a failed create, delete the marker that create wrote first,
    /// unless the record it was creating is there after all. A provider can
    /// apply a create and still fail to answer (a timeout, a response it
    /// cannot read); deleting the marker then would leave the record live but
    /// unmanaged, refused by every retry and cleanup.
    ///
    /// The marker stays when a record with the requested content (its
    /// `desired_fingerprint`) exists, or when the lookup fails. A record with
    /// other content is someone else's — a concurrent create the create-only
    /// write refused — so the marker is deleted rather than left to claim it
    /// should that record ever take the requested value.
    #[allow(clippy::too_many_arguments)]
    async fn clean_up_fresh_marker(
        provider: &dyn DnsProvider,
        zone: &str,
        name: &str,
        record_type: DnsRecordType,
        desired_fingerprint: &str,
        registry_name: &str,
        created_marker: Option<&DnsRecord>,
    ) {
        match provider.get_records(zone, name, record_type).await {
            Ok(records)
                if records.iter().any(|record| {
                    record_fingerprint(&record.content, record.proxied)
                        .is_ok_and(|fingerprint| fingerprint == desired_fingerprint)
                }) =>
            {
                warn!(
                    "Creating {} record '{}' in zone {} reported an error, but the record exists with the requested content; keeping its ownership marker '{}'",
                    record_type, name, zone, registry_name
                );
                return;
            }
            Ok(_) => {}
            Err(error) => {
                warn!(
                    "Creating {} record '{}' in zone {} reported an error, and checking whether the record exists failed too, so its ownership marker '{}' is kept: {}",
                    record_type, name, zone, registry_name, error
                );
                return;
            }
        }
        let marker_records = provider
            .get_records(zone, registry_name, DnsRecordType::TXT)
            .await
            .unwrap_or_default();
        if marker_records.len() == 1
            && created_marker.is_some_and(|created| {
                marker_records[0].id == created.id
                    && marker_records[0].content.canonical() == created.content.canonical()
            })
        {
            if let Err(cleanup_err) = provider.delete_exact_record(zone, &marker_records[0]).await {
                warn!(
                    "Failed to clean up ownership marker '{}' in zone {} after record create failed: {}",
                    registry_name, zone, cleanup_err
                );
            }
        }
    }

    pub(crate) async fn guarded_remove(
        provider: &dyn DnsProvider,
        zone: &str,
        name: &str,
        record_type: DnsRecordType,
        instance: &str,
        signing_key: &[u8; 32],
        scope: OwnershipScope,
    ) -> Result<(), DnsError> {
        Self::require_lossless_writes(provider, zone, name, record_type)?;
        match Self::ownership_of(provider, zone, name, record_type, instance, signing_key).await? {
            RecordOwnership::NotFound => Ok(()),
            RecordOwnership::Orphaned(marker) if !scope.permits(&marker) => {
                Err(scope.refusal(zone, name, record_type, &marker))
            }
            RecordOwnership::Orphaned(_) => {
                let RegistryState::Owned(_, marker_record) =
                    Self::registry_state(provider, zone, name, record_type, instance, signing_key)
                        .await?
                else {
                    return Err(Self::record_conflict(
                        zone,
                        name,
                        record_type,
                        "ownership marker changed while deleting the orphan",
                    ));
                };
                provider.delete_exact_record(zone, &marker_record).await
            }
            RecordOwnership::Owned(_, marker) if !scope.permits(&marker) => {
                Err(scope.refusal(zone, name, record_type, &marker))
            }
            RecordOwnership::Owned(record, _) => {
                let registry =
                    Self::registry_state(provider, zone, name, record_type, instance, signing_key)
                        .await?;
                let RegistryState::Owned(_, marker_record) = registry else {
                    return Err(Self::record_conflict(
                        zone,
                        name,
                        record_type,
                        "ownership marker changed while deleting the record",
                    ));
                };
                provider.delete_exact_record(zone, &record).await?;
                provider.delete_exact_record(zone, &marker_record).await?;
                Ok(())
            }
            RecordOwnership::Unmanaged(_) => Err(DnsError::RecordConflict {
                domain: zone.to_string(),
                name: name.to_string(),
                record_type: record_type.to_string(),
                reason: "the record is not managed by temps, so temps will not delete it"
                    .to_string(),
            }),
            RecordOwnership::OwnedByOther(_, marker) => Err(DnsError::NotOwnedByInstance {
                domain: zone.to_string(),
                name: name.to_string(),
                record_type: record_type.to_string(),
                owner_instance: marker.instance,
            }),
            RecordOwnership::BlockedByOther(marker) => Err(DnsError::NotOwnedByInstance {
                domain: zone.to_string(),
                name: name.to_string(),
                record_type: record_type.to_string(),
                owner_instance: marker.instance,
            }),
            RecordOwnership::RegistryConflict => Err(Self::record_conflict(
                zone,
                name,
                record_type,
                "the reserved ownership registry name contains foreign or ambiguous TXT content",
            )),
        }
    }

    /// Stamp this install's ownership marker onto the record at (name,
    /// type), adopting it in `scope`. With `expected_fingerprint` — the
    /// [`record_fingerprint`] of the record a user reviewed and confirmed —
    /// the record is only adopted while it still has exactly that content, so
    /// a value that changed after the review is never taken over. The caller
    /// holds the record's locks.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn guarded_import(
        provider: &dyn DnsProvider,
        zone: &str,
        name: &str,
        record_type: DnsRecordType,
        expected_fingerprint: Option<&str>,
        instance: &str,
        signing_key: &[u8; 32],
        scope: OwnershipScope,
    ) -> Result<OwnershipMarker, DnsError> {
        Self::require_lossless_writes(provider, zone, name, record_type)?;
        let mut existing = provider.get_records(zone, name, record_type).await?;
        if existing.is_empty() {
            return Err(DnsError::RecordNotFound(format!(
                "{} record '{}' in zone {} does not exist, so it cannot be imported",
                record_type, name, zone
            )));
        }
        if existing.len() != 1 {
            return Err(Self::record_conflict(
                zone,
                name,
                record_type,
                "multiple provider records exist at this name and type; temps manages exactly one record per name and type, so remove the extra records at the provider before importing",
            ));
        }
        let record = existing.remove(0);
        let fingerprint = record_fingerprint(&record.content, record.proxied)?;
        if expected_fingerprint.is_some_and(|expected| expected != fingerprint) {
            return Err(Self::record_conflict(
                zone,
                name,
                record_type,
                &format!(
                    "the record changed after it was reviewed for adoption and is now {}{}, so it was not adopted; preview again to review its current value",
                    record.content.canonical().to_value_string(),
                    if record.proxied { " (proxied)" } else { "" }
                ),
            ));
        }

        match Self::registry_state(provider, zone, name, record_type, instance, signing_key).await? {
            // An existing marker from another workflow/project is never
            // re-stamped by import — that is exactly how one workflow would
            // take over another's record.
            RegistryState::Owned(marker, _) if !scope.permits(&marker) => {
                Err(scope.refusal(zone, name, record_type, &marker))
            }
            RegistryState::Owned(marker, _) if marker.matches_fingerprint(&fingerprint) => {
                Ok(marker)
            }
            RegistryState::Foreign(marker) => Err(DnsError::NotOwnedByInstance {
                domain: zone.to_string(),
                name: name.to_string(),
                record_type: record_type.to_string(),
                owner_instance: marker.instance,
            }),
            RegistryState::Occupied => Err(DnsError::RecordConflict {
                domain: zone.to_string(),
                name: name.to_string(),
                record_type: record_type.to_string(),
                reason: format!(
                    "a TXT record already occupies the ownership registry name '{}' and is not a temps marker; remove it at the provider before importing",
                    registry_record_name(name, record_type)
                ),
            }),
            RegistryState::Absent => {
                let marker = OwnershipMarker::new_signed(
                    signing_key,
                    instance,
                    zone,
                    name,
                    record_type,
                    &fingerprint,
                    scope.project_id,
                    scope.environment_id,
                    scope.controller,
                )?;
                let registry_request =
                    Self::marker_request(&registry_record_name(name, record_type), &marker)?;
                provider.create_record(zone, registry_request).await?;
                Ok(marker)
            }
            // Our own stale marker in a permitted scope: adopt the record's
            // current value by rewriting that exact marker record.
            RegistryState::Owned(stale, marker_record) => {
                let stamp_scope = scope.stamped_over(Some(&stale));
                let marker = OwnershipMarker::new_signed(
                    signing_key,
                    instance,
                    zone,
                    name,
                    record_type,
                    &fingerprint,
                    stamp_scope.project_id,
                    stamp_scope.environment_id,
                    stamp_scope.controller,
                )?;
                let registry_request =
                    Self::marker_request(&registry_record_name(name, record_type), &marker)?;
                Self::rewrite_marker(provider, zone, &marker_record, registry_request).await?;
                Ok(marker)
            }
        }
    }

    fn marker_request(name: &str, marker: &OwnershipMarker) -> Result<DnsRecordRequest, DnsError> {
        Ok(DnsRecordRequest {
            name: name.to_string(),
            content: DnsRecordContent::TXT {
                content: marker.to_txt_content()?,
            },
            ttl: None,
            proxied: false,
        })
    }

    fn record_conflict(
        zone: &str,
        name: &str,
        record_type: DnsRecordType,
        reason: &str,
    ) -> DnsError {
        DnsError::RecordConflict {
            domain: zone.to_string(),
            name: name.to_string(),
            record_type: record_type.to_string(),
            reason: reason.to_string(),
        }
    }

    /// Validate a managed-record request and return its normalized record
    /// name. Content is checked in the canonical form [`Self::guarded_set`]
    /// writes it in (see [`DnsRecordContent::canonical`]): surrounding
    /// whitespace, letter case and a CNAME target's root dot are accepted
    /// and normalized away, anything else invalid is refused.
    pub(crate) fn validate_record_request(
        zone: &str,
        request: &DnsRecordRequest,
    ) -> Result<String, DnsError> {
        let record_type = request.content.record_type();
        Self::validate_record_type(record_type)?;
        if let Some(ttl) = request.ttl {
            if ttl != 1 && !(60..=86_400).contains(&ttl) {
                return Err(DnsError::Validation(format!(
                    "TTL {ttl} is outside the supported range (1 for provider default, or 60..=86400 seconds)"
                )));
            }
        }
        let record_name = request.name.trim();
        match &request.content {
            DnsRecordContent::A { address } => {
                address.trim().parse::<std::net::Ipv4Addr>().map_err(|error| {
                    DnsError::Validation(format!(
                        "Invalid IPv4 address '{address}' for A record '{record_name}' in zone {zone}: {error}"
                    ))
                })?;
            }
            DnsRecordContent::AAAA { address } => {
                address.trim().parse::<std::net::Ipv6Addr>().map_err(|error| {
                    DnsError::Validation(format!(
                        "Invalid IPv6 address '{address}' for AAAA record '{record_name}' in zone {zone}: {error}"
                    ))
                })?;
            }
            DnsRecordContent::CNAME { target } => {
                Self::validate_cname_target(zone, record_name, target)?;
            }
            _ => return Err(DnsError::Validation(format!(
                "Managed DNS records only support A, AAAA, and CNAME; {record_type} is outside the routing-record safety boundary"
            ))),
        }
        Self::validate_record_name(zone, &request.name)
    }

    fn validate_record_type(record_type: DnsRecordType) -> Result<(), DnsError> {
        if matches!(
            record_type,
            DnsRecordType::A | DnsRecordType::AAAA | DnsRecordType::CNAME
        ) {
            Ok(())
        } else {
            Err(DnsError::Validation(format!(
                "Managed DNS records only support A, AAAA, and CNAME; {record_type} is not allowed"
            )))
        }
    }

    pub(crate) fn validate_provider_capabilities(
        capabilities: &crate::providers::DnsProviderCapabilities,
        provider_name: &str,
        record_type: DnsRecordType,
    ) -> Result<(), DnsError> {
        let target_supported = match record_type {
            DnsRecordType::A => capabilities.a_record,
            DnsRecordType::AAAA => capabilities.aaaa_record,
            DnsRecordType::CNAME => capabilities.cname_record,
            _ => false,
        };
        if !target_supported || !capabilities.txt_record {
            return Err(DnsError::NotSupported(format!(
                "DNS provider '{provider_name}' must support both {record_type} and TXT records for ownership-guarded management"
            )));
        }
        Ok(())
    }

    fn validate_record_name(zone: &str, name: &str) -> Result<String, DnsError> {
        let normalized = name.trim().trim_end_matches('.').to_ascii_lowercase();
        let normalized = if normalized.is_empty() {
            "@".to_string()
        } else {
            normalized
        };
        if normalized != "@" {
            let normalized_zone = zone.trim().trim_end_matches('.').to_ascii_lowercase();
            if normalized == normalized_zone || normalized.ends_with(&format!(".{normalized_zone}"))
            {
                return Err(DnsError::Validation(format!(
                    "Record name '{name}' must be relative to managed zone {normalized_zone}, not an FQDN"
                )));
            }
            let first_label = normalized.split('.').next().unwrap_or_default();
            if first_label.starts_with(&OWNERSHIP_REGISTRY_PREFIX.to_ascii_lowercase()) {
                return Err(DnsError::Validation(format!(
                    "Record name '{name}' uses the reserved Temps ownership namespace"
                )));
            }
            Self::validate_relative_labels(&normalized, true, false)?;
        }
        let fqdn_len = if normalized == "@" {
            zone.len()
        } else {
            normalized.len() + 1 + zone.len()
        };
        if fqdn_len > 253 {
            return Err(DnsError::Validation(format!(
                "Record name '{name}' in zone {zone} exceeds the DNS 253-byte FQDN limit"
            )));
        }
        // The companion registry name must fit for EVERY routing type, not
        // just one: `_temps-owned-cname.` is a byte longer than
        // `_temps-owned-aaaa.`, so checking only AAAA let a CNAME's registry
        // FQDN overflow by one byte.
        for record_type in ROUTING_RECORD_TYPES {
            let registry_name = registry_record_name(&normalized, record_type);
            Self::validate_relative_labels(&registry_name, false, true)?;
            if registry_name.len() + 1 + zone.len() > 253 {
                return Err(DnsError::Validation(format!(
                    "Record name '{name}' in zone {zone} exceeds the DNS 253-byte FQDN limit after the {record_type} ownership registry prefix is added"
                )));
            }
        }
        Ok(normalized)
    }

    /// Validate a CNAME target in the canonical form it is written in:
    /// trimmed, lowercase and without the root dot.
    fn validate_cname_target(zone: &str, record_name: &str, target: &str) -> Result<(), DnsError> {
        let canonical = target.trim().trim_end_matches('.').to_ascii_lowercase();
        let invalid = |reason: String| {
            DnsError::Validation(format!(
                "Invalid CNAME target '{target}' for record '{record_name}' in zone {zone}: {reason}"
            ))
        };
        if canonical.is_empty() {
            return Err(invalid(
                "the target is empty; set it to the hostname the record should point at"
                    .to_string(),
            ));
        }
        if canonical.len() > 253 {
            return Err(invalid(format!(
                "the target is {} bytes long, over the DNS 253-byte name limit",
                canonical.len()
            )));
        }
        if let Some(label) = Self::first_invalid_label(&canonical, false, false) {
            return Err(invalid(format!(
                "label '{label}' must be 1-63 letters, digits or hyphens and cannot start or end with a hyphen"
            )));
        }
        Ok(())
    }

    fn validate_relative_labels(
        name: &str,
        allow_wildcard: bool,
        allow_underscore: bool,
    ) -> Result<(), DnsError> {
        match Self::first_invalid_label(name, allow_wildcard, allow_underscore) {
            Some(label) => Err(DnsError::Validation(format!(
                "Invalid DNS label '{label}' in record name '{name}'"
            ))),
            None => Ok(()),
        }
    }

    /// The first label of `name` that is not a valid DNS label: empty,
    /// longer than 63 bytes, starting or ending with a hyphen, or holding
    /// anything but ASCII letters, digits and hyphens (plus underscores when
    /// `allow_underscore`). A leading `*` passes when `allow_wildcard`.
    fn first_invalid_label(
        name: &str,
        allow_wildcard: bool,
        allow_underscore: bool,
    ) -> Option<&str> {
        name.split('.').enumerate().find_map(|(index, label)| {
            let wildcard = allow_wildcard && index == 0 && label == "*";
            let invalid = label.is_empty()
                || label.len() > 63
                || (!wildcard
                    && (label.starts_with('-')
                        || label.ends_with('-')
                        || !label.chars().all(|character| {
                            character.is_ascii_alphanumeric()
                                || character == '-'
                                || (allow_underscore && character == '_')
                        })));
            invalid.then_some(label)
        })
    }
}

// Delivery cleanup: provider-id based access
//
// Domain delivery cleanup resolves the provider and zone from the binding's
// own `dns_provider_id` and `zone` rather than through
// `find_provider_for_domain`, which only matches verified, auto-managed
// zones: a binding must stay removable after its zone stops being either.
// Ownership checks and removal reuse the guarded core above unchanged.
impl ManagedDnsRecordService {
    /// Resolve the provider instance and canonical record location for a
    /// delivery binding, validating the record name and type exactly as
    /// every other guarded operation does.
    async fn delivery_cleanup_target(
        &self,
        provider_id: i32,
        zone: &str,
        name: &str,
        record_type: DnsRecordType,
    ) -> Result<DeliveryCleanupTarget, DnsError> {
        let (provider_model, managed) = self
            .provider_service
            .find_managed_zone_for_delivery_cleanup(provider_id, zone)
            .await?;
        let provider = self
            .provider_service
            .create_provider_instance(&provider_model)?;
        let zone = managed.domain;
        Self::validate_record_type(record_type)?;
        let name = Self::validate_record_name(&zone, name)?;
        Self::validate_provider_capabilities(
            &provider.capabilities(),
            &provider_model.name,
            record_type,
        )?;
        Ok(DeliveryCleanupTarget {
            provider_name: provider_model.name,
            provider,
            zone,
            name,
        })
    }

    /// Ownership state of a delivery binding's record, read through the
    /// binding's own DNS provider and zone (see
    /// [`DnsProviderService::find_managed_zone_for_delivery_cleanup`]).
    pub async fn record_ownership_for_provider(
        &self,
        provider_id: i32,
        zone: &str,
        name: &str,
        record_type: DnsRecordType,
    ) -> Result<RecordOwnership, DnsError> {
        let target = self
            .delivery_cleanup_target(provider_id, zone, name, record_type)
            .await?;
        let instance = self.instance_id().await?;
        Self::ownership_of(
            target.provider.as_ref(),
            &target.zone,
            &target.name,
            record_type,
            &instance,
            &self.signing_key,
        )
        .await
    }

    /// Delete a delivery binding's record through the binding's own DNS
    /// provider and zone. Same ownership guard and locking as
    /// [`Self::remove_managed_record`].
    pub async fn remove_managed_record_for_provider(
        &self,
        provider_id: i32,
        zone: &str,
        name: &str,
        record_type: DnsRecordType,
        scope: OwnershipScope,
    ) -> Result<(), DnsError> {
        self.remove_managed_record_for_provider_with_transaction(
            provider_id,
            zone,
            name,
            record_type,
            scope,
            None,
        )
        .await
    }

    pub(crate) async fn remove_managed_record_for_provider_with_transaction(
        &self,
        provider_id: i32,
        zone: &str,
        name: &str,
        record_type: DnsRecordType,
        scope: OwnershipScope,
        transaction: Option<&DatabaseTransaction>,
    ) -> Result<(), DnsError> {
        let target = self
            .delivery_cleanup_target(provider_id, zone, name, record_type)
            .await?;
        let instance = self.instance_id().await?;
        let _db_lock = if let Some(transaction) = transaction {
            Self::lock_record_on_transaction(transaction, &target.zone, &target.name).await?;
            None
        } else {
            Some(Self::lock_record_in_db(self.db.as_ref(), &target.zone, &target.name).await?)
        };
        let _lease = self.locks.acquire(&target.zone, &target.name).await;
        Self::guarded_remove(
            target.provider.as_ref(),
            &target.zone,
            &target.name,
            record_type,
            &instance,
            &self.signing_key,
            scope,
        )
        .await?;

        info!(
            "Removed managed {} record '{}' in zone {} via provider {} ({}) for delivery cleanup",
            record_type, target.name, target.zone, target.provider_name, provider_id
        );
        Ok(())
    }
}

/// Provider and canonical record location a delivery cleanup acts on.
struct DeliveryCleanupTarget {
    provider_name: String,
    provider: Box<dyn DnsProvider>,
    zone: String,
    name: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{DnsProviderCapabilities, DnsProviderType, DnsZone};
    use async_trait::async_trait;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[tokio::test]
    async fn database_record_lock_serializes_processes_sharing_a_database() {
        let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) => {
                eprintln!("Docker/Postgres unavailable; skipping DNS advisory lock test: {error}");
                return;
            }
        };
        let db = test_db.connection_arc();
        let first = ManagedDnsRecordService::lock_record_in_db(&db, "Example.COM.", "App.")
            .await
            .expect("first writer acquires the lock");
        let conflict = ManagedDnsRecordService::lock_record_in_db(&db, "example.com", "app")
            .await
            .expect_err("same record on another connection must be blocked");
        assert!(
            matches!(conflict, DnsError::RecordLocked { .. }),
            "lock contention must be retryable, not an ownership conflict: {conflict:?}"
        );
        let independent = ManagedDnsRecordService::lock_record_in_db(&db, "example.com", "other")
            .await
            .expect("unrelated record can proceed");
        independent
            .rollback()
            .await
            .expect("release independent lock");
        first.rollback().await.expect("release first lock");
        let next = ManagedDnsRecordService::lock_record_in_db(&db, "example.com", "app")
            .await
            .expect("next writer acquires the released lock");
        next.rollback().await.expect("release next lock");
    }

    /// In-memory provider: records keyed by (name, type). Panics are fine in
    /// tests; production paths never touch this.
    ///
    /// `create_record` is create-only (like a real provider API): it fails
    /// when a record already exists at (name, type). `update_record` replaces.
    struct MockProvider {
        records: Mutex<HashMap<(String, String), DnsRecord>>,
        fail_target_writes: bool,
        /// With `fail_target_writes`, the create is stored first, like a
        /// provider that applies it and then fails to answer.
        apply_failed_target_writes: bool,
        /// With `fail_target_writes`, every listing after the failed create
        /// fails too.
        fail_listings_after_target_failure: bool,
        listings_fail: Mutex<bool>,
        replace_marker_on_target_failure: bool,
        /// Marker (TXT) updates allowed before every later one fails; `None`
        /// never fails them.
        txt_updates_before_failure: Mutex<Option<usize>>,
        /// Target-record updates fail. With `apply_failed_target_updates`
        /// the record is stored first, like a provider that applies a write
        /// and then reports an error (a timeout, a dropped response).
        fail_target_updates: bool,
        apply_failed_target_updates: bool,
        provider_type: DnsProviderType,
        /// Simulates a concurrent writer: inserted into the zone right after
        /// the first TXT (marker) write, i.e. after every ownership check.
        race_record: Mutex<Option<(String, DnsRecordContent)>>,
    }

    impl MockProvider {
        fn new() -> Self {
            Self {
                records: Mutex::new(HashMap::new()),
                fail_target_writes: false,
                apply_failed_target_writes: false,
                fail_listings_after_target_failure: false,
                listings_fail: Mutex::new(false),
                replace_marker_on_target_failure: false,
                txt_updates_before_failure: Mutex::new(None),
                fail_target_updates: false,
                apply_failed_target_updates: false,
                provider_type: DnsProviderType::Cloudflare,
                race_record: Mutex::new(None),
            }
        }

        fn insert_raw(&self, name: &str, content: DnsRecordContent) {
            let record_type = content.record_type().to_string();
            self.records.lock().unwrap().insert(
                (name.to_string(), record_type.clone()),
                DnsRecord {
                    id: Some(format!("{}-{}", name, record_type)),
                    zone: "example.com".to_string(),
                    name: name.to_string(),
                    fqdn: format!("{}.example.com", name),
                    content,
                    ttl: 300,
                    proxied: false,
                    metadata: HashMap::new(),
                },
            );
        }

        fn store(&self, domain: &str, request: DnsRecordRequest) -> DnsRecord {
            let record_type = request.content.record_type();
            let record = DnsRecord {
                id: Some(format!("{}-{}", request.name, record_type)),
                zone: domain.to_string(),
                name: request.name.clone(),
                fqdn: format!("{}.{}", request.name, domain),
                content: request.content,
                ttl: request.ttl.unwrap_or(300),
                proxied: request.proxied,
                metadata: HashMap::new(),
            };
            self.records.lock().unwrap().insert(
                (record.name.clone(), record_type.to_string()),
                record.clone(),
            );
            record
        }

        fn with_record(self, name: &str, content: DnsRecordContent) -> Self {
            self.insert_raw(name, content);
            self
        }

        fn has_record(&self, name: &str, record_type: DnsRecordType) -> bool {
            self.records
                .lock()
                .unwrap()
                .contains_key(&(name.to_string(), record_type.to_string()))
        }

        fn record_value(&self, name: &str, record_type: DnsRecordType) -> Option<String> {
            self.records
                .lock()
                .unwrap()
                .get(&(name.to_string(), record_type.to_string()))
                .map(|r| r.content.to_value_string())
        }
    }

    #[async_trait]
    impl DnsProvider for MockProvider {
        fn provider_type(&self) -> DnsProviderType {
            self.provider_type
        }

        fn capabilities(&self) -> DnsProviderCapabilities {
            DnsProviderCapabilities {
                a_record: true,
                aaaa_record: true,
                cname_record: true,
                txt_record: true,
                proxy: true,
                ..Default::default()
            }
        }

        async fn test_connection(&self) -> Result<bool, DnsError> {
            Ok(true)
        }

        async fn list_zones(&self) -> Result<Vec<DnsZone>, DnsError> {
            Ok(vec![])
        }

        async fn get_zone(&self, _domain: &str) -> Result<Option<DnsZone>, DnsError> {
            Ok(None)
        }

        async fn list_records(&self, _domain: &str) -> Result<Vec<DnsRecord>, DnsError> {
            if *self.listings_fail.lock().unwrap() {
                return Err(DnsError::ApiError("simulated listing failure".to_string()));
            }
            Ok(self.records.lock().unwrap().values().cloned().collect())
        }

        async fn get_record(
            &self,
            _domain: &str,
            name: &str,
            record_type: DnsRecordType,
        ) -> Result<Option<DnsRecord>, DnsError> {
            Ok(self
                .records
                .lock()
                .unwrap()
                .get(&(name.to_string(), record_type.to_string()))
                .cloned())
        }

        async fn create_record(
            &self,
            domain: &str,
            request: DnsRecordRequest,
        ) -> Result<DnsRecord, DnsError> {
            let record_type = request.content.record_type();
            if self.fail_target_writes && record_type != DnsRecordType::TXT {
                if self.replace_marker_on_target_failure {
                    let registry_name = registry_record_name(&request.name, record_type);
                    if let Some(marker) = self
                        .records
                        .lock()
                        .unwrap()
                        .get_mut(&(registry_name, DnsRecordType::TXT.to_string()))
                    {
                        marker.id = Some("other-writer".into());
                        marker.content = DnsRecordContent::TXT {
                            content: "foreign TXT".into(),
                        };
                    }
                }
                if self.apply_failed_target_writes {
                    self.store(domain, request);
                }
                if self.fail_listings_after_target_failure {
                    *self.listings_fail.lock().unwrap() = true;
                }
                return Err(DnsError::ApiError("simulated write failure".to_string()));
            }
            if self
                .records
                .lock()
                .unwrap()
                .contains_key(&(request.name.clone(), record_type.to_string()))
            {
                return Err(DnsError::ApiError(format!(
                    "record {} {} already exists",
                    request.name, record_type
                )));
            }
            let record = self.store(domain, request);
            if record_type == DnsRecordType::TXT {
                if let Some((name, content)) = self.race_record.lock().unwrap().take() {
                    self.insert_raw(&name, content);
                }
            }
            Ok(record)
        }

        async fn update_record(
            &self,
            domain: &str,
            _record_id: &str,
            request: DnsRecordRequest,
        ) -> Result<DnsRecord, DnsError> {
            if request.content.record_type() == DnsRecordType::TXT {
                if let Some(remaining) = self.txt_updates_before_failure.lock().unwrap().as_mut() {
                    if *remaining == 0 {
                        return Err(DnsError::ApiError(
                            "simulated marker write failure".to_string(),
                        ));
                    }
                    *remaining -= 1;
                }
            } else if self.fail_target_updates {
                if self.apply_failed_target_updates {
                    self.store(domain, request);
                }
                return Err(DnsError::ApiError(
                    "simulated record write failure".to_string(),
                ));
            }
            Ok(self.store(domain, request))
        }

        async fn delete_record(&self, _domain: &str, record_id: &str) -> Result<(), DnsError> {
            self.records
                .lock()
                .unwrap()
                .retain(|_, r| r.id.as_deref() != Some(record_id));
            Ok(())
        }
    }

    const INSTANCE: &str = "test-instance";
    const OTHER_INSTANCE: &str = "other-install";
    const SIGNING_KEY: [u8; 32] = [9; 32];

    fn a_request(name: &str, proxied: bool) -> DnsRecordRequest {
        DnsRecordRequest {
            name: name.to_string(),
            content: DnsRecordContent::A {
                address: "192.0.2.10".to_string(),
            },
            ttl: Some(300),
            proxied,
        }
    }

    fn marker_for(record_type: DnsRecordType) -> OwnershipMarker {
        marker_for_instance(INSTANCE, record_type)
    }

    fn marker_for_instance(instance: &str, record_type: DnsRecordType) -> OwnershipMarker {
        marker_for_content(
            instance,
            record_type,
            DnsRecordContent::A {
                address: "192.0.2.10".to_string(),
            },
        )
    }

    fn marker_for_content(
        instance: &str,
        record_type: DnsRecordType,
        content: DnsRecordContent,
    ) -> OwnershipMarker {
        OwnershipMarker::new_signed(
            &SIGNING_KEY,
            instance,
            "example.com",
            "app",
            record_type,
            &record_fingerprint(&content, false).unwrap(),
            Some(1),
            Some(2),
            None,
        )
        .unwrap()
    }

    async fn test_guarded_set(
        provider: &dyn DnsProvider,
        zone: &str,
        request: DnsRecordRequest,
        _marker: &OwnershipMarker,
        instance: &str,
    ) -> Result<DnsRecord, DnsError> {
        ManagedDnsRecordService::guarded_set(
            provider,
            zone,
            request,
            instance,
            &SIGNING_KEY,
            OwnershipScope {
                project_id: Some(1),
                environment_id: Some(2),
                controller: None,
            },
        )
        .await
    }

    async fn test_guarded_remove(
        provider: &dyn DnsProvider,
        zone: &str,
        name: &str,
        record_type: DnsRecordType,
        instance: &str,
    ) -> Result<(), DnsError> {
        test_guarded_remove_scoped(
            provider,
            zone,
            name,
            record_type,
            instance,
            OwnershipScope::default(),
        )
        .await
    }

    async fn test_guarded_remove_scoped(
        provider: &dyn DnsProvider,
        zone: &str,
        name: &str,
        record_type: DnsRecordType,
        instance: &str,
        scope: OwnershipScope,
    ) -> Result<(), DnsError> {
        ManagedDnsRecordService::guarded_remove(
            provider,
            zone,
            name,
            record_type,
            instance,
            &SIGNING_KEY,
            scope,
        )
        .await
    }

    async fn test_guarded_import(
        provider: &dyn DnsProvider,
        zone: &str,
        name: &str,
        record_type: DnsRecordType,
        instance: &str,
        scope: OwnershipScope,
    ) -> Result<OwnershipMarker, DnsError> {
        ManagedDnsRecordService::guarded_import(
            provider,
            zone,
            name,
            record_type,
            None,
            instance,
            &SIGNING_KEY,
            scope,
        )
        .await
    }

    async fn test_ownership_of(
        provider: &dyn DnsProvider,
        zone: &str,
        name: &str,
        record_type: DnsRecordType,
        instance: &str,
    ) -> Result<RecordOwnership, DnsError> {
        ManagedDnsRecordService::ownership_of(
            provider,
            zone,
            name,
            record_type,
            instance,
            &SIGNING_KEY,
        )
        .await
    }

    fn registry_txt(
        name: &str,
        record_type: DnsRecordType,
        marker: &OwnershipMarker,
    ) -> (String, DnsRecordContent) {
        (
            registry_record_name(name, record_type),
            DnsRecordContent::TXT {
                content: marker.to_txt_content().unwrap(),
            },
        )
    }

    // ==================== guarded_set ====================

    #[tokio::test]
    async fn set_creates_record_and_ownership_marker() {
        let provider = MockProvider::new();
        let record = test_guarded_set(
            &provider,
            "example.com",
            a_request("app", false),
            &marker_for(DnsRecordType::A),
            INSTANCE,
        )
        .await
        .unwrap();

        assert_eq!(record.name, "app");
        assert!(provider.has_record("app", DnsRecordType::A));
        assert!(provider.has_record("_temps-owned-a.app", DnsRecordType::TXT));
    }

    #[tokio::test]
    async fn set_refuses_to_overwrite_unmanaged_record() {
        // The core ADR-031 invariant: an existing record without a marker is
        // untouchable, whatever its content.
        let provider = MockProvider::new().with_record(
            "app",
            DnsRecordContent::A {
                address: "203.0.113.1".to_string(),
            },
        );

        let err = test_guarded_set(
            &provider,
            "example.com",
            a_request("app", false),
            &marker_for(DnsRecordType::A),
            INSTANCE,
        )
        .await
        .unwrap_err();

        assert!(matches!(err, DnsError::RecordConflict { .. }));
        // Original record untouched
        assert_eq!(
            provider.record_value("app", DnsRecordType::A).unwrap(),
            "203.0.113.1"
        );
    }

    #[tokio::test]
    async fn set_refuses_to_clobber_user_txt_at_registry_name() {
        // Target record absent, but a NON-marker TXT already lives at the
        // registry name. The marker write must refuse, not upsert over it.
        let provider = MockProvider::new().with_record(
            "_temps-owned-a.app",
            DnsRecordContent::TXT {
                content: "v=spf1 -all".to_string(),
            },
        );

        let err = test_guarded_set(
            &provider,
            "example.com",
            a_request("app", false),
            &marker_for(DnsRecordType::A),
            INSTANCE,
        )
        .await
        .unwrap_err();

        assert!(matches!(err, DnsError::RecordConflict { .. }));
        // The user's TXT is preserved verbatim and no A record was created.
        assert_eq!(
            provider
                .record_value("_temps-owned-a.app", DnsRecordType::TXT)
                .unwrap(),
            "v=spf1 -all"
        );
        assert!(!provider.has_record("app", DnsRecordType::A));
    }

    #[tokio::test]
    async fn set_refuses_foreign_orphan_marker_at_registry_name() {
        // Another install crashed between marker and record: its orphan
        // marker must not be overwritten, or it gets locked out of the name.
        let foreign = marker_for_instance(OTHER_INSTANCE, DnsRecordType::A);
        let (reg_name, reg_content) = registry_txt("app", DnsRecordType::A, &foreign);
        let provider = MockProvider::new().with_record(&reg_name, reg_content);

        let err = test_guarded_set(
            &provider,
            "example.com",
            a_request("app", false),
            &marker_for(DnsRecordType::A),
            INSTANCE,
        )
        .await
        .unwrap_err();

        match err {
            DnsError::NotOwnedByInstance { owner_instance, .. } => {
                assert_eq!(owner_instance, OTHER_INSTANCE);
            }
            other => panic!("expected NotOwnedByInstance, got {:?}", other),
        }
        assert!(!provider.has_record("app", DnsRecordType::A));
    }

    #[tokio::test]
    async fn set_reuses_our_orphan_marker() {
        // WE crashed between marker and record last time: our own orphan
        // marker must not block the retry.
        let (reg_name, reg_content) =
            registry_txt("app", DnsRecordType::A, &marker_for(DnsRecordType::A));
        let provider = MockProvider::new().with_record(&reg_name, reg_content);

        let record = test_guarded_set(
            &provider,
            "example.com",
            a_request("app", false),
            &marker_for(DnsRecordType::A),
            INSTANCE,
        )
        .await
        .unwrap();
        assert_eq!(record.content.to_value_string(), "192.0.2.10");
    }

    #[tokio::test]
    async fn set_refuses_record_owned_by_other_instance() {
        let foreign = marker_for_instance(OTHER_INSTANCE, DnsRecordType::A);
        let (reg_name, reg_content) = registry_txt("app", DnsRecordType::A, &foreign);
        let provider = MockProvider::new()
            .with_record(
                "app",
                DnsRecordContent::A {
                    address: "203.0.113.1".to_string(),
                },
            )
            .with_record(&reg_name, reg_content);

        let err = test_guarded_set(
            &provider,
            "example.com",
            a_request("app", false),
            &marker_for(DnsRecordType::A),
            INSTANCE,
        )
        .await
        .unwrap_err();

        match err {
            DnsError::NotOwnedByInstance { owner_instance, .. } => {
                assert_eq!(owner_instance, OTHER_INSTANCE);
            }
            other => panic!("expected NotOwnedByInstance, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn set_updates_record_owned_by_this_instance() {
        let current_marker = marker_for_content(
            INSTANCE,
            DnsRecordType::A,
            DnsRecordContent::A {
                address: "203.0.113.1".to_string(),
            },
        );
        let (reg_name, reg_content) = registry_txt("app", DnsRecordType::A, &current_marker);
        let provider = MockProvider::new()
            .with_record(
                "app",
                DnsRecordContent::A {
                    address: "203.0.113.1".to_string(),
                },
            )
            .with_record(&reg_name, reg_content);

        let record = test_guarded_set(
            &provider,
            "example.com",
            a_request("app", false),
            &marker_for(DnsRecordType::A),
            INSTANCE,
        )
        .await
        .unwrap();

        assert_eq!(record.content.to_value_string(), "192.0.2.10");
    }

    /// Where an update of an owned record can be interrupted.
    #[derive(Debug, Clone, Copy)]
    enum UpdateFailure {
        /// The marker write that prepares the update fails.
        PrepareMarker,
        /// The record write fails before the provider applies it.
        RecordWrite,
        /// The provider applies the record write, then reports an error.
        AppliedRecordWrite,
        /// The marker write that finalizes the update fails.
        FinalizeMarker,
    }

    async fn ownership_of_app(provider: &MockProvider) -> RecordOwnership {
        ManagedDnsRecordService::ownership_of(
            provider,
            "example.com",
            "app",
            DnsRecordType::A,
            INSTANCE,
            &SIGNING_KEY,
        )
        .await
        .unwrap()
    }

    /// An update interrupted at any step leaves a record this install still
    /// owns, whichever value the provider ended up with, so the next write
    /// goes through and finalizes the marker. A marker still signed for the
    /// old value after the record changed would make the record unmanaged,
    /// and every retry and cleanup would then be refused.
    #[tokio::test]
    async fn interrupted_update_leaves_a_record_this_install_still_owns() {
        let old = DnsRecordContent::A {
            address: "203.0.113.1".to_string(),
        };
        for failure in [
            UpdateFailure::PrepareMarker,
            UpdateFailure::RecordWrite,
            UpdateFailure::AppliedRecordWrite,
            UpdateFailure::FinalizeMarker,
        ] {
            let old_marker = marker_for_content(INSTANCE, DnsRecordType::A, old.clone());
            let (reg_name, reg_content) = registry_txt("app", DnsRecordType::A, &old_marker);
            let mut provider = MockProvider::new()
                .with_record("app", old.clone())
                .with_record(&reg_name, reg_content);
            match failure {
                UpdateFailure::PrepareMarker => {
                    *provider.txt_updates_before_failure.lock().unwrap() = Some(0)
                }
                UpdateFailure::RecordWrite => provider.fail_target_updates = true,
                UpdateFailure::AppliedRecordWrite => {
                    provider.fail_target_updates = true;
                    provider.apply_failed_target_updates = true;
                }
                UpdateFailure::FinalizeMarker => {
                    *provider.txt_updates_before_failure.lock().unwrap() = Some(1)
                }
            }

            let error = test_guarded_set(
                &provider,
                "example.com",
                a_request("app", false),
                &marker_for(DnsRecordType::A),
                INSTANCE,
            )
            .await
            .unwrap_err();
            let stored = match failure {
                UpdateFailure::PrepareMarker | UpdateFailure::RecordWrite => "203.0.113.1",
                UpdateFailure::AppliedRecordWrite | UpdateFailure::FinalizeMarker => "192.0.2.10",
            };
            assert_eq!(
                provider.record_value("app", DnsRecordType::A).as_deref(),
                Some(stored),
                "{failure:?}"
            );
            if matches!(failure, UpdateFailure::FinalizeMarker) {
                assert!(
                    matches!(&error, DnsError::ManagedRecordMarkerNotFinalized(details) if details.stays_managed),
                    "{failure:?}: {error}"
                );
            } else {
                assert!(
                    matches!(error, DnsError::ApiError(_)),
                    "{failure:?}: {error}"
                );
            }
            assert!(
                matches!(
                    ownership_of_app(&provider).await,
                    RecordOwnership::Owned(..)
                ),
                "{failure:?}: the record must stay owned"
            );

            *provider.txt_updates_before_failure.lock().unwrap() = None;
            provider.fail_target_updates = false;
            test_guarded_set(
                &provider,
                "example.com",
                a_request("app", false),
                &marker_for(DnsRecordType::A),
                INSTANCE,
            )
            .await
            .unwrap_or_else(|error| panic!("{failure:?}: the next write must go through: {error}"));
            assert_eq!(
                provider.record_value("app", DnsRecordType::A).as_deref(),
                Some("192.0.2.10"),
                "{failure:?}"
            );
            match ownership_of_app(&provider).await {
                RecordOwnership::Owned(_, marker) => assert!(
                    !marker.has_pending_fingerprint(),
                    "{failure:?}: the completed write finalizes the marker"
                ),
                other => panic!("{failure:?}: expected an owned record, got {other:?}"),
            }
        }
    }

    /// The pending value only covers the value Temps was writing: a record
    /// someone else changed to a third value is unmanaged and is not
    /// overwritten.
    #[tokio::test]
    async fn pending_marker_does_not_cover_a_value_temps_never_wrote() {
        let old = DnsRecordContent::A {
            address: "203.0.113.1".to_string(),
        };
        let pending = marker_for_content(INSTANCE, DnsRecordType::A, old)
            .with_pending_fingerprint(
                &SIGNING_KEY,
                &record_fingerprint(
                    &DnsRecordContent::A {
                        address: "192.0.2.10".to_string(),
                    },
                    false,
                )
                .unwrap(),
            )
            .unwrap();
        let (reg_name, reg_content) = registry_txt("app", DnsRecordType::A, &pending);
        let provider = MockProvider::new()
            .with_record(
                "app",
                DnsRecordContent::A {
                    address: "198.51.100.7".to_string(),
                },
            )
            .with_record(&reg_name, reg_content);

        assert!(matches!(
            ownership_of_app(&provider).await,
            RecordOwnership::Unmanaged(_)
        ));
        let error = test_guarded_set(
            &provider,
            "example.com",
            a_request("app", false),
            &marker_for(DnsRecordType::A),
            INSTANCE,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, DnsError::RecordConflict { .. }), "{error}");
        assert_eq!(
            provider.record_value("app", DnsRecordType::A).as_deref(),
            Some("198.51.100.7")
        );
    }

    #[tokio::test]
    async fn ownership_is_type_scoped_a_marker_does_not_cover_aaaa() {
        // Temps owns `app` A. A user manually maintains `app` AAAA.
        // Writing or removing the AAAA must conflict, not ride on the A marker.
        let (reg_name, reg_content) =
            registry_txt("app", DnsRecordType::A, &marker_for(DnsRecordType::A));
        let provider = MockProvider::new()
            .with_record(
                "app",
                DnsRecordContent::A {
                    address: "192.0.2.10".to_string(),
                },
            )
            .with_record(&reg_name, reg_content)
            .with_record(
                "app",
                DnsRecordContent::AAAA {
                    address: "2001:db8::1".to_string(),
                },
            );

        // Set AAAA → conflict (user's record, no AAAA-scoped marker)
        let err = test_guarded_set(
            &provider,
            "example.com",
            DnsRecordRequest {
                name: "app".to_string(),
                content: DnsRecordContent::AAAA {
                    address: "2001:db8::2".to_string(),
                },
                ttl: None,
                proxied: false,
            },
            &marker_for(DnsRecordType::AAAA),
            INSTANCE,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, DnsError::RecordConflict { .. }));

        // Remove AAAA → conflict
        let err = test_guarded_remove(
            &provider,
            "example.com",
            "app",
            DnsRecordType::AAAA,
            INSTANCE,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, DnsError::RecordConflict { .. }));

        // The user's AAAA is untouched; our A is still updatable.
        assert_eq!(
            provider.record_value("app", DnsRecordType::AAAA).unwrap(),
            "2001:db8::1"
        );
        assert!(test_guarded_set(
            &provider,
            "example.com",
            a_request("app", false),
            &marker_for(DnsRecordType::A),
            INSTANCE,
        )
        .await
        .is_ok());
    }

    #[tokio::test]
    async fn failed_create_cleans_up_fresh_marker() {
        let mut provider = MockProvider::new();
        provider.fail_target_writes = true;

        let err = test_guarded_set(
            &provider,
            "example.com",
            a_request("app", false),
            &marker_for(DnsRecordType::A),
            INSTANCE,
        )
        .await
        .unwrap_err();

        assert!(matches!(err, DnsError::ApiError(_)));
        // No orphan marker left behind for a record that was never created.
        assert!(!provider.has_record("_temps-owned-a.app", DnsRecordType::TXT));
    }

    /// A create the provider applied before reporting an error keeps its
    /// marker: the record stays managed, and the retry goes through.
    #[tokio::test]
    async fn failed_create_keeps_marker_when_the_record_was_created() {
        let mut provider = MockProvider::new();
        provider.fail_target_writes = true;
        provider.apply_failed_target_writes = true;

        let error = test_guarded_set(
            &provider,
            "example.com",
            a_request("app", false),
            &marker_for(DnsRecordType::A),
            INSTANCE,
        )
        .await
        .expect_err("the create reports an error");
        assert!(matches!(error, DnsError::ApiError(_)), "{error}");
        assert!(provider.has_record("_temps-owned-a.app", DnsRecordType::TXT));
        assert!(matches!(
            ownership_of_app(&provider).await,
            RecordOwnership::Owned(..)
        ));

        provider.fail_target_writes = false;
        let record = test_guarded_set(
            &provider,
            "example.com",
            a_request("app", false),
            &marker_for(DnsRecordType::A),
            INSTANCE,
        )
        .await
        .expect("the retry writes the record it already owns");
        assert_eq!(record.content.to_value_string(), "192.0.2.10");
        assert!(matches!(
            ownership_of_app(&provider).await,
            RecordOwnership::Owned(..)
        ));
    }

    /// When it cannot be confirmed that a failed create left no record, its
    /// marker is kept rather than risk leaving a live record unmanaged.
    #[tokio::test]
    async fn failed_create_keeps_marker_when_the_record_cannot_be_checked() {
        let mut provider = MockProvider::new();
        provider.fail_target_writes = true;
        provider.fail_listings_after_target_failure = true;

        let error = test_guarded_set(
            &provider,
            "example.com",
            a_request("app", false),
            &marker_for(DnsRecordType::A),
            INSTANCE,
        )
        .await
        .expect_err("the create reports an error");
        assert!(matches!(error, DnsError::ApiError(_)), "{error}");
        assert!(provider.has_record("_temps-owned-a.app", DnsRecordType::TXT));
    }

    #[tokio::test]
    async fn failed_create_preserves_marker_replaced_by_another_writer() {
        let mut provider = MockProvider::new();
        provider.fail_target_writes = true;
        provider.replace_marker_on_target_failure = true;

        let error = test_guarded_set(
            &provider,
            "example.com",
            a_request("app", false),
            &marker_for(DnsRecordType::A),
            INSTANCE,
        )
        .await
        .expect_err("target write must fail");

        assert!(matches!(error, DnsError::ApiError(_)));
        assert_eq!(
            provider.record_value("_temps-owned-a.app", DnsRecordType::TXT),
            Some("foreign TXT".into()),
            "cleanup must leave a replacement TXT record alone"
        );
    }

    // ==================== guarded_remove ====================

    #[tokio::test]
    async fn remove_refuses_unmanaged_record() {
        let provider = MockProvider::new().with_record(
            "app",
            DnsRecordContent::A {
                address: "203.0.113.1".to_string(),
            },
        );

        let err = test_guarded_remove(&provider, "example.com", "app", DnsRecordType::A, INSTANCE)
            .await
            .unwrap_err();

        assert!(matches!(err, DnsError::RecordConflict { .. }));
        assert!(provider.has_record("app", DnsRecordType::A));
    }

    #[tokio::test]
    async fn remove_deletes_owned_record_and_marker() {
        let (reg_name, reg_content) =
            registry_txt("app", DnsRecordType::A, &marker_for(DnsRecordType::A));
        let provider = MockProvider::new()
            .with_record(
                "app",
                DnsRecordContent::A {
                    address: "192.0.2.10".to_string(),
                },
            )
            .with_record(&reg_name, reg_content);

        test_guarded_remove(&provider, "example.com", "app", DnsRecordType::A, INSTANCE)
            .await
            .unwrap();

        assert!(!provider.has_record("app", DnsRecordType::A));
        assert!(!provider.has_record("_temps-owned-a.app", DnsRecordType::TXT));
    }

    #[tokio::test]
    async fn remove_of_missing_record_is_ok_and_cleans_stray_marker() {
        let (reg_name, reg_content) =
            registry_txt("app", DnsRecordType::A, &marker_for(DnsRecordType::A));
        let provider = MockProvider::new().with_record(&reg_name, reg_content);

        test_guarded_remove(&provider, "example.com", "app", DnsRecordType::A, INSTANCE)
            .await
            .unwrap();

        assert!(!provider.has_record("_temps-owned-a.app", DnsRecordType::TXT));
    }

    #[tokio::test]
    async fn remove_of_missing_record_reports_foreign_marker_and_leaves_it_alone() {
        let foreign = marker_for_instance(OTHER_INSTANCE, DnsRecordType::A);
        let (reg_name, reg_content) = registry_txt("app", DnsRecordType::A, &foreign);
        let provider = MockProvider::new().with_record(&reg_name, reg_content);

        let error =
            test_guarded_remove(&provider, "example.com", "app", DnsRecordType::A, INSTANCE)
                .await
                .unwrap_err();

        assert!(matches!(error, DnsError::NotOwnedByInstance { .. }));
        assert!(provider.has_record(&reg_name, DnsRecordType::TXT));
    }

    // ==================== guarded_import ====================

    #[tokio::test]
    async fn import_stamps_marker_on_unmanaged_record() {
        let provider = MockProvider::new().with_record(
            "app",
            DnsRecordContent::A {
                address: "203.0.113.1".to_string(),
            },
        );

        let imported = test_guarded_import(
            &provider,
            "example.com",
            "app",
            DnsRecordType::A,
            INSTANCE,
            OwnershipScope {
                project_id: Some(1),
                environment_id: Some(2),
                controller: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(imported.project_id, Some(1));
        assert_eq!(imported.record_type, "A");
        assert!(provider.has_record("_temps-owned-a.app", DnsRecordType::TXT));

        // After import, set is allowed.
        let record = test_guarded_set(
            &provider,
            "example.com",
            a_request("app", false),
            &marker_for(DnsRecordType::A),
            INSTANCE,
        )
        .await
        .unwrap();
        assert_eq!(record.content.to_value_string(), "192.0.2.10");
    }

    #[tokio::test]
    async fn import_is_idempotent_when_already_owned() {
        let (reg_name, reg_content) =
            registry_txt("app", DnsRecordType::A, &marker_for(DnsRecordType::A));
        let provider = MockProvider::new()
            .with_record(
                "app",
                DnsRecordContent::A {
                    address: "192.0.2.10".to_string(),
                },
            )
            .with_record(&reg_name, reg_content);

        let marker = test_guarded_import(
            &provider,
            "example.com",
            "app",
            DnsRecordType::A,
            INSTANCE,
            OwnershipScope::default(),
        )
        .await
        .unwrap();
        // Existing marker returned as-is, including its original scope.
        assert_eq!(marker.project_id, Some(1));
    }

    #[tokio::test]
    async fn import_refuses_missing_foreign_and_occupied() {
        // Missing target record
        let provider = MockProvider::new();
        let err = test_guarded_import(
            &provider,
            "example.com",
            "ghost",
            DnsRecordType::A,
            INSTANCE,
            OwnershipScope::default(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, DnsError::RecordNotFound(_)));

        // Foreign marker at registry name
        let foreign = marker_for_instance(OTHER_INSTANCE, DnsRecordType::A);
        let (reg_name, reg_content) = registry_txt("app", DnsRecordType::A, &foreign);
        let provider = MockProvider::new()
            .with_record(
                "app",
                DnsRecordContent::A {
                    address: "203.0.113.1".to_string(),
                },
            )
            .with_record(&reg_name, reg_content);
        let err = test_guarded_import(
            &provider,
            "example.com",
            "app",
            DnsRecordType::A,
            INSTANCE,
            OwnershipScope::default(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, DnsError::NotOwnedByInstance { .. }));

        // Non-marker TXT occupying the registry name: import must refuse
        // rather than clobber the user's TXT.
        let provider = MockProvider::new()
            .with_record(
                "app",
                DnsRecordContent::A {
                    address: "203.0.113.1".to_string(),
                },
            )
            .with_record(
                "_temps-owned-a.app",
                DnsRecordContent::TXT {
                    content: "user-data".to_string(),
                },
            );
        let err = test_guarded_import(
            &provider,
            "example.com",
            "app",
            DnsRecordType::A,
            INSTANCE,
            OwnershipScope::default(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, DnsError::RecordConflict { .. }));
        assert_eq!(
            provider
                .record_value("_temps-owned-a.app", DnsRecordType::TXT)
                .unwrap(),
            "user-data"
        );
    }

    // ==================== ownership_of ====================

    #[tokio::test]
    async fn user_txt_record_at_registry_name_does_not_grant_ownership() {
        // A user TXT that happens to live at the registry name but isn't a
        // valid marker must read as Unmanaged, not Owned.
        let provider = MockProvider::new()
            .with_record(
                "app",
                DnsRecordContent::A {
                    address: "203.0.113.1".to_string(),
                },
            )
            .with_record(
                "_temps-owned-a.app",
                DnsRecordContent::TXT {
                    content: "v=spf1 -all".to_string(),
                },
            );

        let ownership =
            test_ownership_of(&provider, "example.com", "app", DnsRecordType::A, INSTANCE)
                .await
                .unwrap();
        assert!(matches!(ownership, RecordOwnership::Unmanaged(_)));
    }

    // ==================== scope helpers ====================

    const APP_IP: &str = "192.0.2.10";

    fn a_content(address: &str) -> DnsRecordContent {
        DnsRecordContent::A {
            address: address.to_string(),
        }
    }

    fn scoped_marker(
        record_type: DnsRecordType,
        content: DnsRecordContent,
        controller: Option<&'static str>,
        project_id: Option<i32>,
        environment_id: Option<i32>,
    ) -> OwnershipMarker {
        OwnershipMarker::new_signed(
            &SIGNING_KEY,
            INSTANCE,
            "example.com",
            "app",
            record_type,
            &record_fingerprint(&content, false).unwrap(),
            project_id,
            environment_id,
            controller,
        )
        .unwrap()
    }

    const DELIVERY_SCOPE: OwnershipScope = OwnershipScope {
        project_id: Some(1),
        environment_id: Some(2),
        controller: Some("domain-delivery"),
    };

    async fn test_guarded_set_scoped(
        provider: &dyn DnsProvider,
        request: DnsRecordRequest,
        scope: OwnershipScope,
    ) -> Result<DnsRecord, DnsError> {
        ManagedDnsRecordService::guarded_set(
            provider,
            "example.com",
            request,
            INSTANCE,
            &SIGNING_KEY,
            scope,
        )
        .await
    }

    /// `app` A owned by domain delivery (project 1, environment 2).
    fn provider_with_delivery_record() -> MockProvider {
        let marker = scoped_marker(
            DnsRecordType::A,
            a_content(APP_IP),
            Some("domain-delivery"),
            Some(1),
            Some(2),
        );
        let (reg_name, reg_content) = registry_txt("app", DnsRecordType::A, &marker);
        MockProvider::new()
            .with_record("app", a_content(APP_IP))
            .with_record(&reg_name, reg_content)
    }

    #[test]
    fn scope_permits_requires_matching_controller_and_specified_ids() {
        let marker = scoped_marker(
            DnsRecordType::A,
            a_content(APP_IP),
            Some("domain-delivery"),
            Some(1),
            Some(2),
        );
        assert!(DELIVERY_SCOPE.permits(&marker));
        assert!(OwnershipScope::for_controller("domain-delivery").permits(&marker));
        assert!(!OwnershipScope::default().permits(&marker));
        assert!(!OwnershipScope::for_controller("generated-hostname").permits(&marker));
        assert!(!OwnershipScope {
            project_id: Some(3),
            ..DELIVERY_SCOPE
        }
        .permits(&marker));
        assert!(!OwnershipScope {
            environment_id: Some(9),
            ..DELIVERY_SCOPE
        }
        .permits(&marker));

        let generic = scoped_marker(DnsRecordType::A, a_content(APP_IP), None, Some(1), None);
        assert!(OwnershipScope::default().permits(&generic));
        assert!(!DELIVERY_SCOPE.permits(&generic));
    }

    #[test]
    fn stamped_scope_keeps_existing_labels_the_caller_did_not_specify() {
        let marker = scoped_marker(DnsRecordType::A, a_content(APP_IP), None, Some(4), Some(5));
        let stamped = OwnershipScope::default().stamped_over(Some(&marker));
        assert_eq!(stamped.project_id, Some(4));
        assert_eq!(stamped.environment_id, Some(5));
        assert_eq!(stamped.controller, None);

        let explicit = OwnershipScope {
            project_id: Some(4),
            environment_id: Some(6),
            controller: None,
        }
        .stamped_over(Some(&marker));
        assert_eq!(explicit.environment_id, Some(6));
    }

    // ==================== scope enforcement (finding: cross-workflow) ====================

    #[tokio::test]
    async fn generic_scope_cannot_overwrite_domain_delivery_record() {
        let provider = provider_with_delivery_record();

        let mut request = a_request("app", false);
        request.content = a_content("203.0.113.66");
        let error = test_guarded_set_scoped(&provider, request, OwnershipScope::default())
            .await
            .unwrap_err();

        match error {
            DnsError::OwnedByOtherScope(conflict) => {
                assert_eq!(
                    conflict.owner_controller.as_deref(),
                    Some("domain-delivery")
                );
                assert_eq!(conflict.owner_project_id, Some(1));
                assert_eq!(conflict.owner_environment_id, Some(2));
                assert_eq!(conflict.requester_controller, None);
            }
            other => panic!("expected OwnedByOtherScope, got {other:?}"),
        }
        assert_eq!(
            provider.record_value("app", DnsRecordType::A).as_deref(),
            Some(APP_IP)
        );
    }

    #[tokio::test]
    async fn generic_scope_cannot_delete_domain_delivery_record() {
        let provider = provider_with_delivery_record();

        let error =
            test_guarded_remove(&provider, "example.com", "app", DnsRecordType::A, INSTANCE)
                .await
                .unwrap_err();

        assert!(matches!(error, DnsError::OwnedByOtherScope(_)));
        assert!(provider.has_record("app", DnsRecordType::A));
        assert!(provider.has_record("_temps-owned-a.app", DnsRecordType::TXT));
    }

    #[tokio::test]
    async fn other_project_in_same_controller_cannot_overwrite_record() {
        let provider = provider_with_delivery_record();

        let error = test_guarded_set_scoped(
            &provider,
            a_request("app", false),
            OwnershipScope {
                project_id: Some(3),
                environment_id: Some(30),
                controller: Some("domain-delivery"),
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(error, DnsError::OwnedByOtherScope(_)));
    }

    #[tokio::test]
    async fn generic_scope_cannot_import_over_domain_delivery_marker() {
        let provider = provider_with_delivery_record();

        let error = test_guarded_import(
            &provider,
            "example.com",
            "app",
            DnsRecordType::A,
            INSTANCE,
            OwnershipScope::default(),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, DnsError::OwnedByOtherScope(_)));
    }

    #[tokio::test]
    async fn same_scope_can_update_and_delete_domain_delivery_record() {
        let provider = provider_with_delivery_record();

        let mut request = a_request("app", false);
        request.content = a_content("203.0.113.66");
        let record = test_guarded_set_scoped(&provider, request, DELIVERY_SCOPE)
            .await
            .unwrap();
        assert_eq!(record.content.to_value_string(), "203.0.113.66");

        // The committed marker keeps the delivery scope.
        let ownership =
            test_ownership_of(&provider, "example.com", "app", DnsRecordType::A, INSTANCE)
                .await
                .unwrap();
        match ownership {
            RecordOwnership::Owned(_, marker) => {
                assert_eq!(marker.controller.as_deref(), Some("domain-delivery"));
                assert_eq!(marker.project_id, Some(1));
                assert_eq!(marker.environment_id, Some(2));
            }
            other => panic!("expected Owned, got {other:?}"),
        }

        test_guarded_remove_scoped(
            &provider,
            "example.com",
            "app",
            DnsRecordType::A,
            INSTANCE,
            OwnershipScope::for_controller("domain-delivery"),
        )
        .await
        .unwrap();
        assert!(!provider.has_record("app", DnsRecordType::A));
        assert!(!provider.has_record("_temps-owned-a.app", DnsRecordType::TXT));
    }

    #[tokio::test]
    async fn orphan_marker_from_other_scope_is_not_reused_or_removed() {
        // Domain delivery crashed between marker and record. The generic API
        // must neither re-stamp nor delete that orphan.
        let marker = scoped_marker(
            DnsRecordType::A,
            a_content(APP_IP),
            Some("domain-delivery"),
            Some(1),
            Some(2),
        );
        let (reg_name, reg_content) = registry_txt("app", DnsRecordType::A, &marker);
        let provider = MockProvider::new().with_record(&reg_name, reg_content.clone());

        let error = test_guarded_set_scoped(
            &provider,
            a_request("app", false),
            OwnershipScope::default(),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, DnsError::OwnedByOtherScope(_)));
        assert!(!provider.has_record("app", DnsRecordType::A));

        let error =
            test_guarded_remove(&provider, "example.com", "app", DnsRecordType::A, INSTANCE)
                .await
                .unwrap_err();
        assert!(matches!(error, DnsError::OwnedByOtherScope(_)));
        assert_eq!(
            provider.record_value(&reg_name, DnsRecordType::TXT),
            Some(reg_content.to_value_string())
        );

        // The owning workflow can still complete its create.
        let record = test_guarded_set_scoped(&provider, a_request("app", false), DELIVERY_SCOPE)
            .await
            .unwrap();
        assert_eq!(record.content.to_value_string(), APP_IP);
    }

    // ==================== routing siblings ====================

    #[tokio::test]
    async fn create_a_refuses_when_user_owns_aaaa_at_same_name() {
        let provider = MockProvider::new().with_record(
            "app",
            DnsRecordContent::AAAA {
                address: "2001:db8::1".to_string(),
            },
        );

        let error = test_guarded_set(
            &provider,
            "example.com",
            a_request("app", false),
            &marker_for(DnsRecordType::A),
            INSTANCE,
        )
        .await
        .unwrap_err();

        match &error {
            DnsError::RecordConflict { reason, .. } => {
                assert!(reason.contains("AAAA"), "reason must name AAAA: {reason}");
            }
            other => panic!("expected RecordConflict, got {other:?}"),
        }
        assert!(!provider.has_record("app", DnsRecordType::A));
        assert!(!provider.has_record("_temps-owned-a.app", DnsRecordType::TXT));
    }

    #[tokio::test]
    async fn create_aaaa_refuses_when_user_owns_a_or_cname_at_same_name() {
        for existing in [
            a_content("203.0.113.1"),
            DnsRecordContent::CNAME {
                target: "origin.example.net".to_string(),
            },
        ] {
            let existing_type = existing.record_type();
            let provider = MockProvider::new().with_record("app", existing);
            let error = test_guarded_set(
                &provider,
                "example.com",
                DnsRecordRequest {
                    name: "app".to_string(),
                    content: DnsRecordContent::AAAA {
                        address: "2001:db8::2".to_string(),
                    },
                    ttl: None,
                    proxied: false,
                },
                &marker_for(DnsRecordType::AAAA),
                INSTANCE,
            )
            .await
            .unwrap_err();
            match &error {
                DnsError::RecordConflict { reason, .. } => assert!(
                    reason.contains(&format!("a {existing_type} record")),
                    "reason must name {existing_type}: {reason}"
                ),
                other => panic!("expected RecordConflict, got {other:?}"),
            }
            assert!(!provider.has_record("app", DnsRecordType::AAAA));
        }
    }

    #[tokio::test]
    async fn create_aaaa_next_to_own_a_in_same_scope_is_allowed() {
        // Dual-stack: temps owns `app` A for this scope and adds `app` AAAA.
        let (reg_name, reg_content) =
            registry_txt("app", DnsRecordType::A, &marker_for(DnsRecordType::A));
        let provider = MockProvider::new()
            .with_record("app", a_content(APP_IP))
            .with_record(&reg_name, reg_content);

        let record = test_guarded_set(
            &provider,
            "example.com",
            DnsRecordRequest {
                name: "app".to_string(),
                content: DnsRecordContent::AAAA {
                    address: "2001:db8::2".to_string(),
                },
                ttl: None,
                proxied: false,
            },
            &marker_for(DnsRecordType::AAAA),
            INSTANCE,
        )
        .await
        .unwrap();
        assert_eq!(record.content.to_value_string(), "2001:db8::2");
    }

    #[tokio::test]
    async fn create_aaaa_next_to_a_owned_by_other_scope_is_refused() {
        let provider = provider_with_delivery_record();

        let error = test_guarded_set_scoped(
            &provider,
            DnsRecordRequest {
                name: "app".to_string(),
                content: DnsRecordContent::AAAA {
                    address: "2001:db8::2".to_string(),
                },
                ttl: None,
                proxied: false,
            },
            OwnershipScope::default(),
        )
        .await
        .unwrap_err();

        match &error {
            DnsError::RecordConflict { reason, .. } => assert!(reason.contains("a A record")),
            other => panic!("expected RecordConflict, got {other:?}"),
        }
        assert!(!provider.has_record("app", DnsRecordType::AAAA));
    }

    // ==================== create-only fresh writes ====================

    #[tokio::test]
    async fn fresh_create_never_overwrites_a_record_created_concurrently() {
        // Someone creates `app` A after our checks but before our write. The
        // create-only write must fail rather than replace their value.
        let provider = MockProvider::new();
        *provider.race_record.lock().unwrap() =
            Some(("app".to_string(), a_content("203.0.113.50")));

        let error = test_guarded_set(
            &provider,
            "example.com",
            a_request("app", false),
            &marker_for(DnsRecordType::A),
            INSTANCE,
        )
        .await
        .unwrap_err();

        assert!(matches!(error, DnsError::ApiError(_)));
        assert_eq!(
            provider.record_value("app", DnsRecordType::A).as_deref(),
            Some("203.0.113.50")
        );
        // Our just-created marker is cleaned up, so nothing claims their record.
        assert!(!provider.has_record("_temps-owned-a.app", DnsRecordType::TXT));
    }

    #[tokio::test]
    async fn fresh_marker_create_never_overwrites_a_concurrent_registry_txt() {
        // A TXT appears at the registry name between the check and the write:
        // the marker create fails and the TXT survives.
        struct RacingRegistry(MockProvider);

        #[async_trait]
        impl DnsProvider for RacingRegistry {
            fn provider_type(&self) -> DnsProviderType {
                self.0.provider_type()
            }
            fn capabilities(&self) -> DnsProviderCapabilities {
                self.0.capabilities()
            }
            async fn test_connection(&self) -> Result<bool, DnsError> {
                Ok(true)
            }
            async fn list_zones(&self) -> Result<Vec<DnsZone>, DnsError> {
                Ok(vec![])
            }
            async fn get_zone(&self, _domain: &str) -> Result<Option<DnsZone>, DnsError> {
                Ok(None)
            }
            async fn list_records(&self, domain: &str) -> Result<Vec<DnsRecord>, DnsError> {
                self.0.list_records(domain).await
            }
            async fn get_record(
                &self,
                domain: &str,
                name: &str,
                record_type: DnsRecordType,
            ) -> Result<Option<DnsRecord>, DnsError> {
                self.0.get_record(domain, name, record_type).await
            }
            async fn create_record(
                &self,
                domain: &str,
                request: DnsRecordRequest,
            ) -> Result<DnsRecord, DnsError> {
                if request.content.record_type() == DnsRecordType::TXT {
                    self.0.insert_raw(
                        &request.name,
                        DnsRecordContent::TXT {
                            content: "user verification token".to_string(),
                        },
                    );
                }
                self.0.create_record(domain, request).await
            }
            async fn update_record(
                &self,
                domain: &str,
                record_id: &str,
                request: DnsRecordRequest,
            ) -> Result<DnsRecord, DnsError> {
                self.0.update_record(domain, record_id, request).await
            }
            async fn delete_record(&self, domain: &str, record_id: &str) -> Result<(), DnsError> {
                self.0.delete_record(domain, record_id).await
            }
        }

        let provider = RacingRegistry(MockProvider::new());
        let error = test_guarded_set(
            &provider,
            "example.com",
            a_request("app", false),
            &marker_for(DnsRecordType::A),
            INSTANCE,
        )
        .await
        .unwrap_err();

        assert!(matches!(error, DnsError::ApiError(_)));
        assert_eq!(
            provider
                .0
                .record_value("_temps-owned-a.app", DnsRecordType::TXT)
                .as_deref(),
            Some("user verification token")
        );
        assert!(!provider.0.has_record("app", DnsRecordType::A));
    }

    // ==================== case-insensitive names ====================

    #[tokio::test]
    async fn mixed_case_user_record_blocks_lowercase_write() {
        // The user's record is stored as `App`; temps looks up `app`. It must
        // be seen as the same name, not silently get a sibling value.
        let provider = MockProvider::new().with_record("App", a_content("203.0.113.1"));

        let error = test_guarded_set(
            &provider,
            "example.com",
            a_request("app", false),
            &marker_for(DnsRecordType::A),
            INSTANCE,
        )
        .await
        .unwrap_err();

        assert!(matches!(error, DnsError::RecordConflict { .. }));
        assert!(!provider.has_record("app", DnsRecordType::A));
        assert_eq!(
            provider.record_value("App", DnsRecordType::A).as_deref(),
            Some("203.0.113.1")
        );
    }

    // ==================== canonical content (provider echo spelling) ====================

    /// Models Route 53 and Google Cloud DNS: a create or update answers with
    /// the request exactly as it was sent, while every later read lists
    /// CNAME targets lowercased and without the root dot.
    ///
    /// With `respell_echo`, the write response differs from the listing
    /// even for canonical input (the target upper-cased with a root dot), so
    /// the committed ownership marker is exercised against a spelling temps
    /// never sent.
    struct EchoingProvider {
        zone: MockProvider,
        respell_echo: bool,
        /// Every CNAME target the provider was asked to write, as sent.
        written_targets: Mutex<Vec<String>>,
    }

    impl EchoingProvider {
        fn new(respell_echo: bool) -> Self {
            let mut zone = MockProvider::new();
            zone.provider_type = DnsProviderType::Route53;
            Self {
                zone,
                respell_echo,
                written_targets: Mutex::new(Vec::new()),
            }
        }

        fn written_targets(&self) -> Vec<String> {
            self.written_targets.lock().unwrap().clone()
        }

        /// The provider's own normalization, applied to what it stores.
        fn as_listed(content: &DnsRecordContent) -> DnsRecordContent {
            match content {
                DnsRecordContent::CNAME { target } => DnsRecordContent::CNAME {
                    target: target.trim_end_matches('.').to_ascii_lowercase(),
                },
                other => other.clone(),
            }
        }

        /// The provider's answer to a write: the request content as sent.
        fn echo(&self, stored: DnsRecord, sent: DnsRecordContent) -> DnsRecord {
            let content = match sent {
                DnsRecordContent::CNAME { target } if self.respell_echo => {
                    DnsRecordContent::CNAME {
                        target: format!("{}.", target.trim_end_matches('.').to_ascii_uppercase()),
                    }
                }
                sent => sent,
            };
            DnsRecord { content, ..stored }
        }

        fn note_write(&self, content: &DnsRecordContent) {
            if let DnsRecordContent::CNAME { target } = content {
                self.written_targets.lock().unwrap().push(target.clone());
            }
        }
    }

    #[async_trait]
    impl DnsProvider for EchoingProvider {
        fn provider_type(&self) -> DnsProviderType {
            self.zone.provider_type()
        }
        fn capabilities(&self) -> DnsProviderCapabilities {
            self.zone.capabilities()
        }
        async fn test_connection(&self) -> Result<bool, DnsError> {
            Ok(true)
        }
        async fn list_zones(&self) -> Result<Vec<DnsZone>, DnsError> {
            Ok(vec![])
        }
        async fn get_zone(&self, _domain: &str) -> Result<Option<DnsZone>, DnsError> {
            Ok(None)
        }
        async fn list_records(&self, domain: &str) -> Result<Vec<DnsRecord>, DnsError> {
            self.zone.list_records(domain).await
        }
        async fn get_record(
            &self,
            domain: &str,
            name: &str,
            record_type: DnsRecordType,
        ) -> Result<Option<DnsRecord>, DnsError> {
            self.zone.get_record(domain, name, record_type).await
        }
        async fn create_record(
            &self,
            domain: &str,
            request: DnsRecordRequest,
        ) -> Result<DnsRecord, DnsError> {
            self.note_write(&request.content);
            let sent = request.content.clone();
            let stored = self
                .zone
                .create_record(
                    domain,
                    DnsRecordRequest {
                        content: Self::as_listed(&request.content),
                        ..request
                    },
                )
                .await?;
            Ok(self.echo(stored, sent))
        }
        async fn update_record(
            &self,
            domain: &str,
            record_id: &str,
            request: DnsRecordRequest,
        ) -> Result<DnsRecord, DnsError> {
            self.note_write(&request.content);
            let sent = request.content.clone();
            let stored = self
                .zone
                .update_record(
                    domain,
                    record_id,
                    DnsRecordRequest {
                        content: Self::as_listed(&request.content),
                        ..request
                    },
                )
                .await?;
            Ok(self.echo(stored, sent))
        }
        async fn delete_record(&self, domain: &str, record_id: &str) -> Result<(), DnsError> {
            self.zone.delete_record(domain, record_id).await
        }
    }

    fn cname_request(name: &str, target: &str) -> DnsRecordRequest {
        DnsRecordRequest {
            name: name.to_string(),
            content: DnsRecordContent::CNAME {
                target: target.to_string(),
            },
            ttl: None,
            proxied: false,
        }
    }

    async fn assert_cname_owned(provider: &dyn DnsProvider, expected_target: &str) {
        match test_ownership_of(
            provider,
            "example.com",
            "app",
            DnsRecordType::CNAME,
            INSTANCE,
        )
        .await
        .unwrap()
        {
            RecordOwnership::Owned(record, _) => assert_eq!(
                record.content,
                DnsRecordContent::CNAME {
                    target: expected_target.to_string()
                }
            ),
            other => panic!("expected the CNAME to be Owned, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cname_created_through_a_verbatim_echo_provider_can_be_updated_and_deleted() {
        // A reported failure: the marker fingerprinted the create response
        // (`Origin.Example.NET.`), the next read listed `origin.example.net`,
        // and update/delete then refused the record as unmanaged.
        let provider = EchoingProvider::new(false);

        let created = test_guarded_set(
            &provider,
            "example.com",
            cname_request("app", "Origin.Example.NET."),
            &marker_for(DnsRecordType::CNAME),
            INSTANCE,
        )
        .await
        .unwrap();
        // Temps writes the canonical spelling, so the echo already is it.
        assert_eq!(
            created.content,
            DnsRecordContent::CNAME {
                target: "origin.example.net".to_string()
            }
        );
        assert_cname_owned(&provider, "origin.example.net").await;

        test_guarded_set(
            &provider,
            "example.com",
            cname_request("app", "Edge.Example.NET."),
            &marker_for(DnsRecordType::CNAME),
            INSTANCE,
        )
        .await
        .unwrap();
        assert_cname_owned(&provider, "edge.example.net").await;
        assert_eq!(
            provider.written_targets(),
            vec![
                "origin.example.net".to_string(),
                "edge.example.net".to_string()
            ]
        );

        test_guarded_remove(
            &provider,
            "example.com",
            "app",
            DnsRecordType::CNAME,
            INSTANCE,
        )
        .await
        .unwrap();
        assert!(!provider.zone.has_record("app", DnsRecordType::CNAME));
        assert!(!provider
            .zone
            .has_record("_temps-owned-cname.app", DnsRecordType::TXT));
    }

    #[tokio::test]
    async fn marker_matches_listing_when_the_write_response_is_spelled_differently() {
        // Even for canonical input, a write response spelled differently
        // from the provider's listing must not break ownership.
        let provider = EchoingProvider::new(true);

        let created = test_guarded_set(
            &provider,
            "example.com",
            cname_request("app", "origin.example.net"),
            &marker_for(DnsRecordType::CNAME),
            INSTANCE,
        )
        .await
        .unwrap();
        assert_eq!(created.content.to_value_string(), "ORIGIN.EXAMPLE.NET.");
        assert_eq!(
            provider
                .zone
                .record_value("app", DnsRecordType::CNAME)
                .as_deref(),
            Some("origin.example.net")
        );
        assert_cname_owned(&provider, "origin.example.net").await;

        test_guarded_set(
            &provider,
            "example.com",
            cname_request("app", "edge.example.net"),
            &marker_for(DnsRecordType::CNAME),
            INSTANCE,
        )
        .await
        .unwrap();
        assert_cname_owned(&provider, "edge.example.net").await;

        test_guarded_remove(
            &provider,
            "example.com",
            "app",
            DnsRecordType::CNAME,
            INSTANCE,
        )
        .await
        .unwrap();
        assert!(!provider.zone.has_record("app", DnsRecordType::CNAME));
        assert!(!provider
            .zone
            .has_record("_temps-owned-cname.app", DnsRecordType::TXT));
    }

    #[tokio::test]
    async fn guarded_set_writes_canonical_addresses() {
        let provider = MockProvider::new();

        test_guarded_set(
            &provider,
            "example.com",
            DnsRecordRequest {
                name: "app".to_string(),
                content: DnsRecordContent::AAAA {
                    address: "2001:DB8:0:0:0:0:0:1".to_string(),
                },
                ttl: None,
                proxied: false,
            },
            &marker_for(DnsRecordType::AAAA),
            INSTANCE,
        )
        .await
        .unwrap();

        assert_eq!(
            provider.record_value("app", DnsRecordType::AAAA).as_deref(),
            Some("2001:db8::1")
        );
        assert!(matches!(
            test_ownership_of(
                &provider,
                "example.com",
                "app",
                DnsRecordType::AAAA,
                INSTANCE
            )
            .await
            .unwrap(),
            RecordOwnership::Owned(..)
        ));
    }

    #[test]
    fn record_request_validation_accepts_spellings_that_canonicalize() {
        for content in [
            DnsRecordContent::CNAME {
                target: " Origin.Example.NET. ".to_string(),
            },
            DnsRecordContent::A {
                address: " 192.0.2.10 ".to_string(),
            },
            DnsRecordContent::AAAA {
                address: "2001:DB8::1".to_string(),
            },
        ] {
            let request = DnsRecordRequest {
                name: "App".to_string(),
                content,
                ttl: None,
                proxied: false,
            };
            assert_eq!(
                ManagedDnsRecordService::validate_record_request("example.com", &request).unwrap(),
                "app"
            );
        }
    }

    #[test]
    fn record_request_validation_rejects_empty_and_invalid_targets_with_context() {
        for (target, expected) in [
            ("", "is empty"),
            ("  .  ", "is empty"),
            ("-origin.example.net", "label '-origin'"),
            ("origin..example.net", "label ''"),
            ("origin_1.example.net", "label 'origin_1'"),
        ] {
            match ManagedDnsRecordService::validate_record_request(
                "example.com",
                &cname_request("app", target),
            ) {
                Err(DnsError::Validation(message)) => {
                    assert!(message.contains(expected), "{target:?}: {message}");
                    assert!(message.contains("record 'app'"), "{target:?}: {message}");
                    assert!(
                        message.contains("zone example.com"),
                        "{target:?}: {message}"
                    );
                }
                other => panic!("{target:?}: expected a Validation error, got {other:?}"),
            }
        }

        let request = DnsRecordRequest {
            name: "app".to_string(),
            content: DnsRecordContent::A {
                address: "203.0.113.300".to_string(),
            },
            ttl: None,
            proxied: false,
        };
        match ManagedDnsRecordService::validate_record_request("example.com", &request) {
            Err(DnsError::Validation(message)) => {
                assert!(message.contains("'203.0.113.300'"), "{message}");
                assert!(message.contains("A record 'app'"), "{message}");
                assert!(message.contains("zone example.com"), "{message}");
            }
            other => panic!("expected a Validation error, got {other:?}"),
        }
    }

    // ==================== whole-zone writers ====================

    #[tokio::test]
    async fn whole_zone_writer_providers_are_refused_for_every_guarded_write() {
        let mut provider = MockProvider::new().with_record("legacy", a_content("203.0.113.9"));
        provider.provider_type = DnsProviderType::Namecheap;

        let error = test_guarded_set(
            &provider,
            "example.com",
            a_request("app", false),
            &marker_for(DnsRecordType::A),
            INSTANCE,
        )
        .await
        .unwrap_err();
        match &error {
            DnsError::NotSupported(message) => {
                assert!(message.contains("namecheap"), "{message}");
                assert!(message.contains("whole zone"), "{message}");
                assert!(message.contains("'app'"), "{message}");
            }
            other => panic!("expected NotSupported, got {other:?}"),
        }
        assert!(!provider.has_record("app", DnsRecordType::A));
        assert!(!provider.has_record("_temps-owned-a.app", DnsRecordType::TXT));

        assert!(matches!(
            test_guarded_remove(
                &provider,
                "example.com",
                "legacy",
                DnsRecordType::A,
                INSTANCE
            )
            .await,
            Err(DnsError::NotSupported(_))
        ));
        assert!(matches!(
            test_guarded_import(
                &provider,
                "example.com",
                "legacy",
                DnsRecordType::A,
                INSTANCE,
                OwnershipScope::default(),
            )
            .await,
            Err(DnsError::NotSupported(_))
        ));
        assert!(provider.has_record("legacy", DnsRecordType::A));
        assert!(!provider.has_record("_temps-owned-a.legacy", DnsRecordType::TXT));
    }

    // ==================== validate_record_name ====================

    #[test]
    fn record_name_length_accounts_for_longest_registry_prefix() {
        // "_temps-owned-cname." is 19 bytes, one more than the AAAA prefix.
        // With zone "example.com" (11 bytes) the CNAME registry FQDN is
        // 19 + len(name) + 1 + 11, so the longest accepted name is 222 bytes.
        let label = "a".repeat(63);
        let base = format!("{label}.{label}.{label}");
        let fits = format!("{base}.{}", "b".repeat(222 - base.len() - 1));
        let overflows = format!("{base}.{}", "b".repeat(223 - base.len() - 1));
        assert_eq!(fits.len(), 222);
        assert_eq!(overflows.len(), 223);

        assert_eq!(
            ManagedDnsRecordService::validate_record_name("example.com", &fits).unwrap(),
            fits
        );
        // 18 + 223 + 1 + 11 = 253 fits the AAAA registry name, but the CNAME
        // one is 254 bytes and must be rejected.
        match ManagedDnsRecordService::validate_record_name("example.com", &overflows) {
            Err(DnsError::Validation(message)) => {
                assert!(message.contains("CNAME"), "{message}");
            }
            other => panic!("expected Validation error, got {other:?}"),
        }
    }

    // ==================== KeyedLocks ====================

    #[tokio::test]
    async fn keyed_locks_normalize_case_and_trailing_dot() {
        let locks = Arc::new(KeyedLocks::new());
        let lease = locks.acquire("Example.COM.", " App. ").await;
        {
            let map = locks.inner.lock().unwrap();
            assert!(map.contains_key(&("example.com".to_string(), "app".to_string())));
        }
        // The same record spelled differently must wait for the first lease.
        let contender = {
            let locks = locks.clone();
            tokio::spawn(async move {
                let _lease = locks.acquire("example.com", "app").await;
            })
        };
        tokio::task::yield_now().await;
        assert!(!contender.is_finished());
        assert_eq!(locks.inner.lock().unwrap().len(), 1);
        drop(lease);
        contender.await.unwrap();
        assert!(locks.inner.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn keyed_locks_serialize_same_key_and_clean_up() {
        let locks = Arc::new(KeyedLocks::new());
        let lease = locks.acquire("example.com", "app").await;
        assert_eq!(locks.inner.lock().unwrap().len(), 1);
        drop(lease);
        assert!(locks.inner.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn keyed_lock_cleanup_is_cancellation_safe() {
        let locks = Arc::new(KeyedLocks::new());
        let task_locks = locks.clone();
        let task = tokio::spawn(async move {
            let _lease = task_locks.acquire("example.com", "cancelled").await;
            std::future::pending::<()>().await;
        });
        tokio::task::yield_now().await;
        assert_eq!(locks.inner.lock().unwrap().len(), 1);
        task.abort();
        let _ = task.await;
        assert!(locks.inner.lock().unwrap().is_empty());
    }

    // ==================== instance_id (MockDatabase) ====================

    fn identity_row(id: &str) -> dns_instance_identity::Model {
        dns_instance_identity::Model {
            id: 1,
            instance_id: id.to_string(),
            created_at: chrono::Utc::now(),
        }
    }

    fn service_with_db(db: sea_orm::DatabaseConnection) -> ManagedDnsRecordService {
        let db = Arc::new(db);
        let encryption = Arc::new(
            temps_core::EncryptionService::new("0123456789abcdef0123456789abcdef")
                .expect("32-byte test key"),
        );
        let provider_service = Arc::new(DnsProviderService::new(db.clone(), encryption.clone()));
        ManagedDnsRecordService::new(db, provider_service, encryption)
    }

    #[tokio::test]
    async fn instance_id_returns_existing_row() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![identity_row("existing-id")]])
            .into_connection();
        let service = service_with_db(db);

        assert_eq!(service.instance_id().await.unwrap(), "existing-id");
        // Cached: a second call must not hit the DB again (mock has no more
        // results queued and would error).
        assert_eq!(service.instance_id().await.unwrap(), "existing-id");
    }

    #[tokio::test]
    async fn instance_id_creates_row_on_first_use() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // find → empty
            .append_query_results(vec![Vec::<dns_instance_identity::Model>::new()])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 1,
                rows_affected: 1,
            }])
            // insert RETURNING → the created row
            .append_query_results(vec![vec![identity_row("fresh-id")]])
            .into_connection();
        let service = service_with_db(db);

        assert_eq!(service.instance_id().await.unwrap(), "fresh-id");
    }

    #[tokio::test]
    async fn instance_id_recovers_when_losing_insert_race() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // find → empty
            .append_query_results(vec![Vec::<dns_instance_identity::Model>::new()])
            // insert → unique violation
            .append_exec_errors(vec![sea_orm::DbErr::Custom(
                "duplicate key value violates unique constraint".to_string(),
            )])
            // re-find → winner's row
            .append_query_results(vec![vec![identity_row("winner-id")]])
            .into_connection();
        let service = service_with_db(db);

        assert_eq!(service.instance_id().await.unwrap(), "winner-id");
    }
}
