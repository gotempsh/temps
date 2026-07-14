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
//! The ownership marker TXT is written BEFORE the target record. A crash
//! between the two leaves a harmless orphan marker (which this install may
//! later reuse or clean up), never a live unmarked record that a later run
//! would refuse to manage.
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
//! Ownership is per (name, type), and `remove_record` deletes every value at
//! that name+type. If a user manually adds a second A value to a
//! temps-managed name (DNS round-robin), temps removal deletes that value
//! too. Values under an owned name+type are treated as one owned unit; users
//! must not hand-edit temps-managed names (the marker makes them
//! discoverable).

use std::collections::HashMap;
use std::sync::Arc;

use sea_orm::{ActiveModelTrait, ActiveValue::Set, DatabaseConnection, EntityTrait};
use temps_entities::dns_instance_identity;
use tracing::{info, warn};

use crate::errors::DnsError;
use crate::ownership::{check_proxy_allowed, registry_record_name, OwnershipMarker};
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
    Owned(OwnershipMarker),
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

    fn get(&self, zone: &str, name: &str) -> Arc<tokio::sync::Mutex<()>> {
        // Poison-proof: the critical section is a plain HashMap op that can't
        // panic, but if it somehow did, recovering the map beats turning every
        // future DNS write into a panic until restart.
        let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        map.entry((zone.to_string(), name.to_string()))
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// Drop the map entry if no one else holds the Arc (map + caller = 2).
    fn release(&self, zone: &str, name: &str, handle: Arc<tokio::sync::Mutex<()>>) {
        let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if Arc::strong_count(&handle) == 2 {
            map.remove(&(zone.to_string(), name.to_string()));
        }
    }
}

/// Ownership-guarded record management on top of [`DnsProviderService`].
pub struct ManagedDnsRecordService {
    db: Arc<DatabaseConnection>,
    provider_service: Arc<DnsProviderService>,
    instance_id: tokio::sync::OnceCell<String>,
    locks: KeyedLocks,
}

impl ManagedDnsRecordService {
    pub fn new(db: Arc<DatabaseConnection>, provider_service: Arc<DnsProviderService>) -> Self {
        Self {
            db,
            provider_service,
            instance_id: tokio::sync::OnceCell::new(),
            locks: KeyedLocks::new(),
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
        scope: OwnershipScope,
    ) -> Result<DnsRecord, DnsError> {
        let (provider_model, managed) = self
            .provider_service
            .find_provider_for_domain(domain)
            .await?
            .ok_or_else(|| DnsError::DomainNotManaged(domain.to_string()))?;
        let provider = self
            .provider_service
            .create_provider_instance(&provider_model)?;
        let zone = managed.domain.clone();

        request.proxied = request.proxied || managed.proxied_by_default;
        if request.proxied {
            check_proxy_allowed(
                &provider.capabilities(),
                &provider_model.name,
                &zone,
                &request.name,
            )?;
        }

        let instance = self.instance_id().await?;
        let marker = OwnershipMarker::new(
            &instance,
            request.content.record_type(),
            scope.project_id,
            scope.environment_id,
        );

        let name = request.name.clone();
        let lock = self.locks.get(&zone, &name);
        let record = {
            let _guard = lock.lock().await;
            Self::guarded_set(provider.as_ref(), &zone, request, &marker, &instance).await
        };
        self.locks.release(&zone, &name, lock);
        let record = record?;

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

        let instance = self.instance_id().await?;
        let lock = self.locks.get(&zone, name);
        let result = {
            let _guard = lock.lock().await;
            Self::guarded_remove(provider.as_ref(), &zone, name, record_type, &instance).await
        };
        self.locks.release(&zone, name, lock);
        result?;

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
        let (provider_model, managed) = self
            .provider_service
            .find_provider_for_domain(domain)
            .await?
            .ok_or_else(|| DnsError::DomainNotManaged(domain.to_string()))?;
        let provider = self
            .provider_service
            .create_provider_instance(&provider_model)?;
        let zone = managed.domain.clone();

        let instance = self.instance_id().await?;
        let lock = self.locks.get(&zone, name);
        let marker = {
            let _guard = lock.lock().await;
            Self::guarded_import(
                provider.as_ref(),
                &zone,
                name,
                record_type,
                &instance,
                scope,
            )
            .await
        };
        self.locks.release(&zone, name, lock);
        let marker = marker?;

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
        let instance = self.instance_id().await?;

        Self::ownership_of(
            provider.as_ref(),
            &managed.domain,
            name,
            record_type,
            &instance,
        )
        .await
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
    ) -> Result<RegistryState, DnsError> {
        let registry_name = registry_record_name(record_name, record_type);
        let txt = provider
            .get_record(zone, &registry_name, DnsRecordType::TXT)
            .await?;
        let Some(record) = txt else {
            return Ok(RegistryState::Absent);
        };
        let DnsRecordContent::TXT { content } = &record.content else {
            return Ok(RegistryState::Occupied);
        };
        Ok(match OwnershipMarker::parse(content) {
            None => RegistryState::Occupied,
            Some(marker) if marker.covers(instance, record_type) => RegistryState::Owned(marker),
            Some(marker) if !marker.is_owned_by(instance) => RegistryState::Foreign(marker),
            // Parses, is ours, but doesn't cover this record type — temps
            // never writes that at a type-scoped name; treat as untouchable.
            Some(_) => RegistryState::Occupied,
        })
    }

    async fn ownership_of(
        provider: &dyn DnsProvider,
        zone: &str,
        name: &str,
        record_type: DnsRecordType,
        instance: &str,
    ) -> Result<RecordOwnership, DnsError> {
        let existing = provider.get_record(zone, name, record_type).await?;
        let Some(record) = existing else {
            return Ok(RecordOwnership::NotFound);
        };
        match Self::registry_state(provider, zone, name, record_type, instance).await? {
            RegistryState::Owned(marker) => Ok(RecordOwnership::Owned(record, marker)),
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
        marker: &OwnershipMarker,
        instance: &str,
    ) -> Result<DnsRecord, DnsError> {
        let record_type = request.content.record_type();
        let existing = provider
            .get_record(zone, &request.name, record_type)
            .await?;
        let registry =
            Self::registry_state(provider, zone, &request.name, record_type, instance).await?;

        // Both the target record AND the registry name must be free or ours.
        match (&existing, &registry) {
            // Update of a record we own, or create where our (possibly
            // orphaned) marker already sits.
            (_, RegistryState::Owned(_)) => {}
            // Fresh create: nothing at either name.
            (None, RegistryState::Absent) => {}
            (Some(_), RegistryState::Absent | RegistryState::Occupied) => {
                return Err(DnsError::RecordConflict {
                    domain: zone.to_string(),
                    name: request.name.clone(),
                    record_type: record_type.to_string(),
                    reason: "an existing record with this name is not managed by temps".to_string(),
                });
            }
            (None, RegistryState::Occupied) => {
                return Err(DnsError::RecordConflict {
                    domain: zone.to_string(),
                    name: request.name.clone(),
                    record_type: record_type.to_string(),
                    reason: format!(
                        "a TXT record already occupies the ownership registry name '{}' and is not a temps marker",
                        registry_record_name(&request.name, record_type)
                    ),
                });
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

        // Marker first: a crash after this point leaves an orphan TXT (noise,
        // reusable by us), never a live unmarked record (a permanent conflict
        // against ourselves).
        let registry_name = registry_record_name(&request.name, record_type);
        let registry_request = DnsRecordRequest {
            name: registry_name.clone(),
            content: DnsRecordContent::TXT {
                content: marker.to_txt_content()?,
            },
            ttl: request.ttl,
            proxied: false,
        };
        provider.set_record(zone, registry_request).await?;

        match provider.set_record(zone, request.clone()).await {
            Ok(record) => Ok(record),
            Err(e) => {
                // Creating the target failed. If nothing existed before, the
                // fresh marker is pure junk — clean it up best-effort.
                if existing.is_none() {
                    if let Err(cleanup_err) = provider
                        .remove_record(zone, &registry_name, DnsRecordType::TXT)
                        .await
                    {
                        warn!(
                            "Failed to clean up ownership marker '{}' in zone {} after record create failed: {}",
                            registry_name, zone, cleanup_err
                        );
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
    ) -> Result<(), DnsError> {
        match Self::ownership_of(provider, zone, name, record_type, instance).await? {
            RecordOwnership::NotFound => {
                // Record already gone; clean up a stray marker of ours if the
                // registry still has one so it doesn't accumulate. Foreign or
                // occupied registry names are left untouched.
                if let RegistryState::Owned(_) =
                    Self::registry_state(provider, zone, name, record_type, instance).await?
                {
                    provider
                        .remove_record(
                            zone,
                            &registry_record_name(name, record_type),
                            DnsRecordType::TXT,
                        )
                        .await?;
                }
                Ok(())
            }
            RecordOwnership::Owned(_, _) => {
                provider.remove_record(zone, name, record_type).await?;
                provider
                    .remove_record(
                        zone,
                        &registry_record_name(name, record_type),
                        DnsRecordType::TXT,
                    )
                    .await?;
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
        }
    }

    async fn guarded_import(
        provider: &dyn DnsProvider,
        zone: &str,
        name: &str,
        record_type: DnsRecordType,
        instance: &str,
        scope: OwnershipScope,
    ) -> Result<OwnershipMarker, DnsError> {
        let existing = provider.get_record(zone, name, record_type).await?;
        if existing.is_none() {
            return Err(DnsError::RecordNotFound(format!(
                "{} record '{}' in zone {} does not exist, so it cannot be imported",
                record_type, name, zone
            )));
        }

        match Self::registry_state(provider, zone, name, record_type, instance).await? {
            RegistryState::Owned(marker) => Ok(marker), // already ours — idempotent
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
                let marker = OwnershipMarker::new(
                    instance,
                    record_type,
                    scope.project_id,
                    scope.environment_id,
                );
                let registry_request = DnsRecordRequest {
                    name: registry_record_name(name, record_type),
                    content: DnsRecordContent::TXT {
                        content: marker.to_txt_content()?,
                    },
                    ttl: None,
                    proxied: false,
                };
                provider.set_record(zone, registry_request).await?;
                Ok(marker)
            }
        }
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
        OwnershipMarker::new(INSTANCE, record_type, Some(1), Some(2))
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
        let record = ManagedDnsRecordService::guarded_set(
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

        let err = ManagedDnsRecordService::guarded_set(
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

        let err = ManagedDnsRecordService::guarded_set(
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
        let foreign = OwnershipMarker::new(OTHER_INSTANCE, DnsRecordType::A, None, None);
        let (reg_name, reg_content) = registry_txt("app", DnsRecordType::A, &foreign);
        let provider = MockProvider::new().with_record(&reg_name, reg_content);

        let err = ManagedDnsRecordService::guarded_set(
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

        let record = ManagedDnsRecordService::guarded_set(
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
        let foreign = OwnershipMarker::new(OTHER_INSTANCE, DnsRecordType::A, None, None);
        let (reg_name, reg_content) = registry_txt("app", DnsRecordType::A, &foreign);
        let provider = MockProvider::new()
            .with_record(
                "app",
                DnsRecordContent::A {
                    address: "203.0.113.1".to_string(),
                },
            )
            .with_record(&reg_name, reg_content);

        let err = ManagedDnsRecordService::guarded_set(
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
        let (reg_name, reg_content) =
            registry_txt("app", DnsRecordType::A, &marker_for(DnsRecordType::A));
        let provider = MockProvider::new()
            .with_record(
                "app",
                DnsRecordContent::A {
                    address: "203.0.113.1".to_string(),
                },
            )
            .with_record(&reg_name, reg_content);

        let record = ManagedDnsRecordService::guarded_set(
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
        let err = ManagedDnsRecordService::guarded_set(
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
        let err = ManagedDnsRecordService::guarded_remove(
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
        assert!(ManagedDnsRecordService::guarded_set(
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

        let err = ManagedDnsRecordService::guarded_set(
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

        let err = ManagedDnsRecordService::guarded_remove(
            &provider,
            "example.com",
            "app",
            DnsRecordType::A,
            INSTANCE,
        )
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

        ManagedDnsRecordService::guarded_remove(
            &provider,
            "example.com",
            "app",
            DnsRecordType::A,
            INSTANCE,
        )
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

        ManagedDnsRecordService::guarded_remove(
            &provider,
            "example.com",
            "app",
            DnsRecordType::A,
            INSTANCE,
        )
        .await
        .unwrap();

        assert!(!provider.has_record("_temps-owned-a.app", DnsRecordType::TXT));
    }

    #[tokio::test]
    async fn remove_of_missing_record_leaves_foreign_marker_alone() {
        let foreign = OwnershipMarker::new(OTHER_INSTANCE, DnsRecordType::A, None, None);
        let (reg_name, reg_content) = registry_txt("app", DnsRecordType::A, &foreign);
        let provider = MockProvider::new().with_record(&reg_name, reg_content);

        ManagedDnsRecordService::guarded_remove(
            &provider,
            "example.com",
            "app",
            DnsRecordType::A,
            INSTANCE,
        )
        .await
        .unwrap();

        // Their orphan marker survives.
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

        let imported = ManagedDnsRecordService::guarded_import(
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
        assert_eq!(imported.record_type.as_deref(), Some("A"));
        assert!(provider.has_record("_temps-owned-a.app", DnsRecordType::TXT));

        // After import, set is allowed.
        let record = ManagedDnsRecordService::guarded_set(
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

        let marker = ManagedDnsRecordService::guarded_import(
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
        let err = ManagedDnsRecordService::guarded_import(
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
        let foreign = OwnershipMarker::new(OTHER_INSTANCE, DnsRecordType::A, None, None);
        let (reg_name, reg_content) = registry_txt("app", DnsRecordType::A, &foreign);
        let provider = MockProvider::new()
            .with_record(
                "app",
                DnsRecordContent::A {
                    address: "203.0.113.1".to_string(),
                },
            )
            .with_record(&reg_name, reg_content);
        let err = ManagedDnsRecordService::guarded_import(
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
        let err = ManagedDnsRecordService::guarded_import(
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

        let ownership = ManagedDnsRecordService::ownership_of(
            &provider,
            "example.com",
            "app",
            DnsRecordType::A,
            INSTANCE,
        )
        .await
        .unwrap();
        assert!(matches!(ownership, RecordOwnership::Unmanaged(_)));
    }

    // ==================== KeyedLocks ====================

    #[tokio::test]
    async fn keyed_locks_serialize_same_key_and_clean_up() {
        let locks = Arc::new(KeyedLocks::new());

        // Same key returns the same lock; different keys don't contend.
        let a1 = locks.get("example.com", "app");
        let a2 = locks.get("example.com", "app");
        let b = locks.get("example.com", "other");
        assert!(Arc::ptr_eq(&a1, &a2));
        assert!(!Arc::ptr_eq(&a1, &b));

        // Serialization: hold a1, second locker must not acquire until drop.
        let guard = a1.lock().await;
        assert!(a2.try_lock().is_err());
        drop(guard);
        assert!(a2.try_lock().is_ok());

        // Cleanup: after all handles released, the map entry is gone.
        locks.release("example.com", "other", b);
        locks.release("example.com", "app", a1);
        assert_eq!(
            locks.inner.lock().unwrap().len(),
            1,
            "app entry still held via a2"
        );
        locks.release("example.com", "app", a2);
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
        let provider_service = Arc::new(DnsProviderService::new(db.clone(), encryption));
        ManagedDnsRecordService::new(db, provider_service)
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
