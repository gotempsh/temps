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
//! against the remote zone is unavoidable: a record created by someone else
//! between our ownership check and our write can still be overwritten. That
//! residual window is accepted — closing it is impossible without provider
//! transactions. What IS controlled: all guarded operations on the same
//! (zone, record name) within this process are serialized through a keyed
//! async lock, so temps never races itself.
//!
//! # Removal granularity
//!
//! Ownership is per exact provider record. Multiple values at the same
//! (name, type) are treated as a conflict, and deletion uses the provider's
//! exact record identifier so unrelated values are never removed.

use std::collections::HashMap;
use std::sync::Arc;

use sea_orm::{ActiveModelTrait, ActiveValue::Set, DatabaseConnection, EntityTrait};
use temps_entities::{dns_instance_identity, environments, projects};
use tracing::{info, warn};

use crate::errors::DnsError;
use crate::ownership::{
    check_proxy_allowed, record_fingerprint, registry_record_name, OwnershipMarker,
    OWNERSHIP_REGISTRY_PREFIX,
};
use crate::providers::{DnsProvider, DnsRecord, DnsRecordContent, DnsRecordRequest, DnsRecordType};
use crate::services::provider_service::DnsProviderService;

/// What a managed record was created for; stamped into the ownership marker
/// so the provider-side registry shows which project/environment a record
/// belongs to.
#[derive(Debug, Clone, Copy, Default)]
pub struct OwnershipScope {
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
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
        // Poison-proof: the critical section is a plain HashMap op that can't
        // panic, but if it somehow did, recovering the map beats turning every
        // future DNS write into a panic until restart.
        let key = (zone.to_string(), name.to_string());
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

struct KeyedLockLease {
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
            locks: Arc::new(KeyedLocks::new()),
        }
    }

    /// Get (or create on first use) this install's ownership instance ID.
    ///
    /// The ID never rotates once created — rotating would orphan every record
    /// this install previously stamped.
    pub async fn instance_id(&self) -> Result<String, DnsError> {
        let id = self
            .instance_id
            .get_or_try_init(|| async {
                if let Some(row) = dns_instance_identity::Entity::find()
                    .one(self.db.as_ref())
                    .await?
                {
                    return Ok::<String, DnsError>(row.instance_id);
                }

                let fresh = uuid::Uuid::new_v4().to_string();
                let row = dns_instance_identity::ActiveModel {
                    id: Set(1),
                    instance_id: Set(fresh),
                    ..Default::default()
                };
                // Two concurrent first writes can race on the single-row PK;
                // whoever loses re-reads the winner's ID instead of failing.
                match row.insert(self.db.as_ref()).await {
                    Ok(created) => Ok(created.instance_id),
                    Err(insert_err) => dns_instance_identity::Entity::find()
                        .one(self.db.as_ref())
                        .await?
                        .map(|row| row.instance_id)
                        .ok_or(DnsError::Database(insert_err)),
                }
            })
            .await?;
        Ok(id.clone())
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
        mut request: DnsRecordRequest,
        proxied_override: Option<bool>,
        scope: OwnershipScope,
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

    /// Delete a managed record. Refuses unless this install owns it.
    pub async fn remove_managed_record(
        &self,
        domain: &str,
        name: &str,
        record_type: DnsRecordType,
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
        let _lease = self.locks.acquire(&zone, &name).await;
        Self::guarded_remove(
            provider.as_ref(),
            &zone,
            &name,
            record_type,
            &instance,
            &self.signing_key,
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
        let _lease = self.locks.acquire(&zone, &name).await;
        let marker = Self::guarded_import(
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
        Ok(match OwnershipMarker::parse(content) {
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

    async fn ownership_of(
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

    async fn guarded_set(
        provider: &dyn DnsProvider,
        zone: &str,
        request: DnsRecordRequest,
        instance: &str,
        signing_key: &[u8; 32],
        scope: OwnershipScope,
    ) -> Result<DnsRecord, DnsError> {
        let record_type = request.content.record_type();
        let mut existing = provider
            .get_records(zone, &request.name, record_type)
            .await?;
        if existing.len() > 1 {
            return Err(Self::record_conflict(
                zone,
                &request.name,
                record_type,
                "multiple provider records exist at this name and type",
            ));
        }
        let existing = existing.pop();
        let desired_fingerprint = record_fingerprint(&request.content, request.proxied)?;
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
            (Some(record), RegistryState::Owned(marker, _)) => {
                let current = record_fingerprint(&record.content, record.proxied)?;
                if !marker.matches_fingerprint(&current) {
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
            (Some(_), RegistryState::Absent | RegistryState::Occupied) => {
                return Err(Self::record_conflict(
                    zone,
                    &request.name,
                    record_type,
                    "an existing record with this name is not managed by temps",
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

        let registry_name = registry_record_name(&request.name, record_type);
        let marker = OwnershipMarker::new_signed(
            signing_key,
            instance,
            zone,
            &request.name,
            record_type,
            &desired_fingerprint,
            scope.project_id,
            scope.environment_id,
        )?;
        let registry_request = Self::marker_request(&registry_name, &marker)?;

        // Creates are marker-first. Updates keep the old, correctly-bound
        // marker until the provider mutation succeeds, so a failed update
        // never grants authority over content that was not written.
        let fresh_create = existing.is_none();
        if fresh_create {
            provider.set_record(zone, registry_request.clone()).await?;
        }

        match provider.set_record(zone, request).await {
            Ok(record) => {
                let actual_fingerprint = record_fingerprint(&record.content, record.proxied)?;
                let committed_marker = OwnershipMarker::new_signed(
                    signing_key,
                    instance,
                    zone,
                    &record.name,
                    record_type,
                    &actual_fingerprint,
                    scope.project_id,
                    scope.environment_id,
                )?;
                provider
                    .set_record(
                        zone,
                        Self::marker_request(&registry_name, &committed_marker)?,
                    )
                    .await?;
                Ok(record)
            }
            Err(e) => {
                if fresh_create {
                    if let RegistryState::Absent = registry {
                        let marker_records = provider
                            .get_records(zone, &registry_name, DnsRecordType::TXT)
                            .await
                            .unwrap_or_default();
                        if marker_records.len() == 1 {
                            if let Err(cleanup_err) =
                                provider.delete_exact_record(zone, &marker_records[0]).await
                            {
                                warn!(
                            "Failed to clean up ownership marker '{}' in zone {} after record create failed: {}",
                            registry_name, zone, cleanup_err
                        );
                            }
                        }
                    }
                }
                Err(e)
            }
        }
    }

    async fn guarded_remove(
        provider: &dyn DnsProvider,
        zone: &str,
        name: &str,
        record_type: DnsRecordType,
        instance: &str,
        signing_key: &[u8; 32],
    ) -> Result<(), DnsError> {
        match Self::ownership_of(provider, zone, name, record_type, instance, signing_key).await? {
            RecordOwnership::NotFound => Ok(()),
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

    async fn guarded_import(
        provider: &dyn DnsProvider,
        zone: &str,
        name: &str,
        record_type: DnsRecordType,
        instance: &str,
        signing_key: &[u8; 32],
        scope: OwnershipScope,
    ) -> Result<OwnershipMarker, DnsError> {
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
                "multiple provider records exist at this name and type",
            ));
        }
        let record = existing.remove(0);
        let fingerprint = record_fingerprint(&record.content, record.proxied)?;

        match Self::registry_state(provider, zone, name, record_type, instance, signing_key).await? {
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
            RegistryState::Absent | RegistryState::Owned(_, _) => {
                let marker = OwnershipMarker::new_signed(signing_key, instance, zone, name, record_type, &fingerprint, scope.project_id, scope.environment_id)?;
                let registry_request = Self::marker_request(&registry_record_name(name, record_type), &marker)?;
                provider.set_record(zone, registry_request).await?;
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

    fn validate_record_request(zone: &str, request: &DnsRecordRequest) -> Result<String, DnsError> {
        let record_type = request.content.record_type();
        Self::validate_record_type(record_type)?;
        if let Some(ttl) = request.ttl {
            if ttl != 1 && !(60..=86_400).contains(&ttl) {
                return Err(DnsError::Validation(format!(
                    "TTL {ttl} is outside the supported range (1 for provider default, or 60..=86400 seconds)"
                )));
            }
        }
        match &request.content {
            DnsRecordContent::A { address } => {
                address.parse::<std::net::Ipv4Addr>().map_err(|error| {
                    DnsError::Validation(format!("Invalid IPv4 address '{address}': {error}"))
                })?;
            }
            DnsRecordContent::AAAA { address } => {
                address.parse::<std::net::Ipv6Addr>().map_err(|error| {
                    DnsError::Validation(format!("Invalid IPv6 address '{address}': {error}"))
                })?;
            }
            DnsRecordContent::CNAME { target } => {
                Self::validate_absolute_dns_name(target)?;
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

    fn validate_provider_capabilities(
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
            Self::validate_relative_labels(&normalized, true)?;
        }
        let registry_name = registry_record_name(&normalized, DnsRecordType::AAAA);
        Self::validate_relative_labels(&registry_name, false)?;
        let fqdn_len = if normalized == "@" {
            zone.len()
        } else {
            normalized.len() + 1 + zone.len()
        };
        if fqdn_len > 253 || registry_name.len() + 1 + zone.len() > 253 {
            return Err(DnsError::Validation(format!(
                "Record name '{name}' exceeds the DNS 253-byte FQDN limit after ownership metadata is added"
            )));
        }
        Ok(normalized)
    }

    fn validate_absolute_dns_name(name: &str) -> Result<(), DnsError> {
        let normalized = name.trim().trim_end_matches('.').to_ascii_lowercase();
        if normalized.is_empty() || normalized.len() > 253 {
            return Err(DnsError::Validation(format!(
                "Invalid CNAME target '{name}'"
            )));
        }
        Self::validate_relative_labels(&normalized, false)
    }

    fn validate_relative_labels(name: &str, allow_wildcard: bool) -> Result<(), DnsError> {
        for (index, label) in name.split('.').enumerate() {
            let wildcard = allow_wildcard && index == 0 && label == "*";
            if label.is_empty()
                || label.len() > 63
                || (!wildcard
                    && (label.starts_with('-')
                        || label.ends_with('-')
                        || !label.chars().all(|character| {
                            character.is_ascii_alphanumeric()
                                || character == '-'
                                || character == '_'
                        })))
            {
                return Err(DnsError::Validation(format!(
                    "Invalid DNS label '{label}' in record name '{name}'"
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{DnsProviderCapabilities, DnsProviderType, DnsZone};
    use async_trait::async_trait;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// In-memory provider: records keyed by (name, type). Panics are fine in
    /// tests; production paths never touch this.
    struct MockProvider {
        records: Mutex<HashMap<(String, String), DnsRecord>>,
        fail_target_writes: bool,
    }

    impl MockProvider {
        fn new() -> Self {
            Self {
                records: Mutex::new(HashMap::new()),
                fail_target_writes: false,
            }
        }

        fn with_record(self, name: &str, content: DnsRecordContent) -> Self {
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
            DnsProviderType::Manual
        }

        fn capabilities(&self) -> DnsProviderCapabilities {
            DnsProviderCapabilities {
                a_record: true,
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
                return Err(DnsError::ApiError("simulated write failure".to_string()));
            }
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
            Ok(record)
        }

        async fn update_record(
            &self,
            domain: &str,
            _record_id: &str,
            request: DnsRecordRequest,
        ) -> Result<DnsRecord, DnsError> {
            self.create_record(domain, request).await
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
        ManagedDnsRecordService::guarded_remove(
            provider,
            zone,
            name,
            record_type,
            instance,
            &SIGNING_KEY,
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
                project_id: Some(9),
                environment_id: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(imported.project_id, Some(9));
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

    // ==================== KeyedLocks ====================

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
