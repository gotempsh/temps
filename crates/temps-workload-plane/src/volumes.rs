// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Volume rows (ADR-055 §12): `workload_volumes` and
//! `workload_volume_attachments`, and their conversion to
//! [`temps_workload_volumes::Volume`].
//!
//! A volume belongs to a product object (`environment/3`), not to one
//! workload: revisions come and go, the data stays. When the plane checks a
//! spec's mounts it lends the owner's volumes to the spec's own
//! [`WorkloadRef`], which is what [`temps_workload_volumes::validate_mounts`]
//! compares.
//!
//! Every read is one indexed query per owner; nothing here scans the table.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection, DbErr,
    EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, Set, Statement, TransactionTrait,
};
use temps_entities::{workload_volume_attachments as attachments, workload_volumes as volumes};
use temps_workload::{VolumeKinds, VolumeMount, WorkloadRef};
use temps_workload_volumes::{
    on_owner_delete, pinning, validate_mounts, validate_name, Backing, BackingKind, OnOwnerDelete,
    Volume, VolumeError, VolumePin,
};

use crate::plane::Owner;

/// Default and maximum page sizes of [`VolumeStore::list`].
pub const DEFAULT_PAGE_SIZE: u64 = 20;
pub const MAX_PAGE_SIZE: u64 = 100;

#[derive(Debug, thiserror::Error)]
pub enum VolumeStoreError {
    #[error("volume {volume_id} of {owner} not found")]
    NotFound { volume_id: i64, owner: String },

    #[error("{owner} already has a volume named '{name}'")]
    NameTaken { owner: String, name: String },

    #[error("{owner} already mounts a volume at '{mount_path}' (volume '{other}')")]
    MountPathTaken {
        owner: String,
        mount_path: String,
        other: String,
    },

    #[error("volume '{name}' of {owner}: {source}")]
    Invalid {
        owner: String,
        name: String,
        #[source]
        source: Box<VolumeError>,
    },

    #[error("volume {volume_id} of {owner}: {message}")]
    Validation {
        volume_id: i64,
        owner: String,
        message: String,
    },

    #[error("volume {volume_id}: stored {field} is invalid: {reason}")]
    Corrupt {
        volume_id: i64,
        field: &'static str,
        reason: String,
    },

    #[error("database error while {operation} (volumes of {owner}): {source}")]
    Database {
        operation: &'static str,
        owner: String,
        #[source]
        source: DbErr,
    },
}

fn db(operation: &'static str, owner: &Owner) -> impl FnOnce(DbErr) -> VolumeStoreError {
    let owner = owner.to_string();
    move |source| VolumeStoreError::Database {
        operation,
        owner,
        source,
    }
}

/// Where an owner's workloads mount a volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeAttachment {
    pub mount_path: String,
    pub read_only: bool,
}

/// One volume row, typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeRecord {
    pub id: i64,
    pub owner: Owner,
    pub name: String,
    pub backing: Backing,
    pub node_id: Option<i32>,
    pub size_mb: Option<u64>,
    pub retain_on_delete: bool,
    pub attachment: Option<VolumeAttachment>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl VolumeRecord {
    /// The volume as `temps-workload-volumes` checks it, lent to `owner`
    /// (the workload that mounts it).
    pub fn as_volume(&self, owner: WorkloadRef) -> Volume {
        Volume {
            id: self.id,
            name: self.name.clone(),
            owner,
            backing: self.backing.clone(),
            node_id: self.node_id,
            size_mb: self.size_mb,
            retain_on_delete: self.retain_on_delete,
        }
    }

    /// The mount a spec carries for this volume's attachment, with the bind
    /// source resolved.
    pub fn mount(&self) -> Option<VolumeMount> {
        self.attachment.as_ref().map(|a| VolumeMount {
            volume: self.name.clone(),
            mount_path: a.mount_path.clone(),
            read_only: a.read_only,
            source: Some(bind_source(&self.backing).to_string()),
        })
    }
}

/// What the runtime binds for `backing`.
pub fn bind_source(backing: &Backing) -> &str {
    match backing {
        Backing::Named { name } => name,
        Backing::BlockImage { path } | Backing::PreopenedDir { path } => path,
    }
}

/// The deterministic Docker volume name of a named volume: unique per live
/// owner and name, and recognisable on the host (`docker volume ls`).
pub fn named_backing_ref(owner: &Owner, name: &str) -> String {
    format!("temps-vol-{}-{}-{}", owner.kind, owner.id, name)
}

/// A volume to create.
#[derive(Debug, Clone)]
pub struct NewVolume {
    pub owner: Owner,
    pub name: String,
    pub backing: BackingKind,
    /// Pre-place the volume on a node; `None` creates it where its first
    /// workload lands.
    pub node_id: Option<i32>,
    pub size_mb: Option<u64>,
    pub retain_on_delete: bool,
}

/// What a delete did to one volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletedVolume {
    pub volume: VolumeRecord,
    /// `Delete`: the caller removes the backing; `Retain`: it is kept.
    pub backing: OnOwnerDelete,
}

fn decode(
    row: volumes::Model,
    attachment: Option<attachments::Model>,
) -> Result<VolumeRecord, VolumeStoreError> {
    let backing = match row.backing.as_str() {
        "named" => Backing::Named {
            name: row.backing_ref.clone(),
        },
        "block_image" => Backing::BlockImage {
            path: row.backing_ref.clone(),
        },
        "preopened_dir" => Backing::PreopenedDir {
            path: row.backing_ref.clone(),
        },
        other => {
            return Err(VolumeStoreError::Corrupt {
                volume_id: row.id,
                field: "backing",
                reason: format!("unknown backing '{other}'"),
            })
        }
    };
    let size_mb = match row.size_mb {
        None => None,
        Some(size) => Some(u64::try_from(size).map_err(|_| VolumeStoreError::Corrupt {
            volume_id: row.id,
            field: "size_mb",
            reason: format!("negative size {size}"),
        })?),
    };
    Ok(VolumeRecord {
        id: row.id,
        owner: Owner::new(row.owner_kind, row.owner_id),
        name: row.name,
        backing,
        node_id: row.node_id,
        size_mb,
        retain_on_delete: row.retain_on_delete,
        attachment: attachment.map(|a| VolumeAttachment {
            mount_path: a.mount_path,
            read_only: a.read_only,
        }),
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

/// The volume tables. Cheap to clone; holds only the connection.
#[derive(Clone)]
pub struct VolumeStore {
    db: Arc<DatabaseConnection>,
}

impl VolumeStore {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Create a volume row. The backing itself is created by a
    /// [`temps_workload_volumes::VolumeProvider`] when a workload first
    /// mounts it.
    pub async fn create(&self, new: NewVolume) -> Result<VolumeRecord, VolumeStoreError> {
        let owner_text = new.owner.to_string();
        validate_name(&new.name).map_err(|source| VolumeStoreError::Invalid {
            owner: owner_text.clone(),
            name: new.name.clone(),
            source: Box::new(source),
        })?;
        if new.backing != BackingKind::Named {
            return Err(VolumeStoreError::Invalid {
                owner: owner_text,
                name: new.name.clone(),
                source: Box::new(VolumeError::UnsupportedBacking {
                    driver: "docker".to_string(),
                    volume: new.name,
                    backing: new.backing,
                }),
            });
        }
        if let Some(node) = new.node_id.filter(|node| *node < 0) {
            return Err(VolumeStoreError::Validation {
                volume_id: 0,
                owner: owner_text,
                message: format!("node {node} is not a node id (0 is the control plane)"),
            });
        }
        let size_mb = match new.size_mb {
            Some(0) => {
                return Err(VolumeStoreError::Validation {
                    volume_id: 0,
                    owner: owner_text,
                    message: "size_mb must be positive when set".to_string(),
                })
            }
            Some(size) => Some(
                i64::try_from(size).map_err(|_| VolumeStoreError::Validation {
                    volume_id: 0,
                    owner: owner_text.clone(),
                    message: format!("size_mb {size} is too large"),
                })?,
            ),
            None => None,
        };
        let taken = volumes::Entity::find()
            .filter(volumes::Column::OwnerKind.eq(new.owner.kind.as_str()))
            .filter(volumes::Column::OwnerId.eq(new.owner.id))
            .filter(volumes::Column::Name.eq(new.name.as_str()))
            .filter(volumes::Column::DeletedAt.is_null())
            .count(self.db.as_ref())
            .await
            .map_err(db("checking the volume name", &new.owner))?;
        if taken > 0 {
            return Err(VolumeStoreError::NameTaken {
                owner: owner_text,
                name: new.name,
            });
        }
        let now = Utc::now();
        let row = volumes::ActiveModel {
            owner_kind: Set(new.owner.kind.clone()),
            owner_id: Set(new.owner.id),
            name: Set(new.name.clone()),
            backing: Set(new.backing.as_str().to_string()),
            backing_ref: Set(named_backing_ref(&new.owner, &new.name)),
            node_id: Set(new.node_id),
            size_mb: Set(size_mb),
            retain_on_delete: Set(new.retain_on_delete),
            created_at: Set(now),
            updated_at: Set(now),
            deleted_at: Set(None),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await
        .map_err(|source| {
            // The partial unique index: a concurrent create won.
            if source
                .to_string()
                .contains("workload_volumes_owner_name_live")
            {
                VolumeStoreError::NameTaken {
                    owner: new.owner.to_string(),
                    name: new.name.clone(),
                }
            } else {
                VolumeStoreError::Database {
                    operation: "inserting the volume",
                    owner: new.owner.to_string(),
                    source,
                }
            }
        })?;
        tracing::info!(volume_id = row.id, owner = %new.owner, name = %row.name, "volume created");
        decode(row, None)
    }

    /// The owner's live volumes, newest first, with their attachments: one
    /// query. `page` is 1-based; `page_size` defaults to 20, at most 100.
    pub async fn list(
        &self,
        owner: &Owner,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<(Vec<VolumeRecord>, u64), VolumeStoreError> {
        let page = page.unwrap_or(1).max(1);
        let page_size = page_size
            .unwrap_or(DEFAULT_PAGE_SIZE)
            .clamp(1, MAX_PAGE_SIZE);
        let query = volumes::Entity::find()
            .filter(volumes::Column::OwnerKind.eq(owner.kind.as_str()))
            .filter(volumes::Column::OwnerId.eq(owner.id))
            .filter(volumes::Column::DeletedAt.is_null());
        let total = query
            .clone()
            .count(self.db.as_ref())
            .await
            .map_err(db("counting the owner's volumes", owner))?;
        let rows = query
            .find_also_related(attachments::Entity)
            .order_by_desc(volumes::Column::CreatedAt)
            .order_by_desc(volumes::Column::Id)
            .paginate(self.db.as_ref(), page_size)
            .fetch_page(page - 1)
            .await
            .map_err(db("listing the owner's volumes", owner))?;
        let records = rows
            .into_iter()
            .map(|(row, attachment)| decode(row, attachment))
            .collect::<Result<Vec<_>, _>>()?;
        Ok((records, total))
    }

    /// Every live volume of `owner` (bounded by what one owner creates),
    /// with attachments, for a create: one query.
    pub async fn all_of(&self, owner: &Owner) -> Result<Vec<VolumeRecord>, VolumeStoreError> {
        volumes::Entity::find()
            .filter(volumes::Column::OwnerKind.eq(owner.kind.as_str()))
            .filter(volumes::Column::OwnerId.eq(owner.id))
            .filter(volumes::Column::DeletedAt.is_null())
            .find_also_related(attachments::Entity)
            .order_by_asc(volumes::Column::Id)
            .all(self.db.as_ref())
            .await
            .map_err(db("reading the owner's volumes", owner))?
            .into_iter()
            .map(|(row, attachment)| decode(row, attachment))
            .collect()
    }

    /// The owner's attached volumes as the mounts a spec carries, sources
    /// resolved. Empty for an owner with no attachment.
    pub async fn mounts_of(&self, owner: &Owner) -> Result<Vec<VolumeMount>, VolumeStoreError> {
        Ok(self
            .all_of(owner)
            .await?
            .iter()
            .filter_map(VolumeRecord::mount)
            .collect())
    }

    pub async fn get(
        &self,
        owner: &Owner,
        volume_id: i64,
    ) -> Result<VolumeRecord, VolumeStoreError> {
        let found = volumes::Entity::find_by_id(volume_id)
            .filter(volumes::Column::OwnerKind.eq(owner.kind.as_str()))
            .filter(volumes::Column::OwnerId.eq(owner.id))
            .filter(volumes::Column::DeletedAt.is_null())
            .find_also_related(attachments::Entity)
            .one(self.db.as_ref())
            .await
            .map_err(db("reading the volume", owner))?;
        match found {
            Some((row, attachment)) => decode(row, attachment),
            None => Err(VolumeStoreError::NotFound {
                volume_id,
                owner: owner.to_string(),
            }),
        }
    }

    /// Mount volume `volume_id` at `mount_path` in every workload of its
    /// owner. Replaces an existing attachment of the same volume.
    pub async fn attach(
        &self,
        owner: &Owner,
        volume_id: i64,
        mount_path: &str,
        read_only: bool,
    ) -> Result<VolumeRecord, VolumeStoreError> {
        let volume = self.get(owner, volume_id).await?;
        // The same checks admission runs, against this one mount and every
        // other attachment of the owner, so a bad path is refused here and
        // not at the next deploy.
        let mount = VolumeMount {
            volume: volume.name.clone(),
            mount_path: mount_path.to_string(),
            read_only,
            source: None,
        };
        let lent = WorkloadRef::new(temps_workload::WorkloadKind::Deployment, 0);
        let all = self.all_of(owner).await?;
        let mut mounts: Vec<VolumeMount> = all
            .iter()
            .filter(|other| other.id != volume_id)
            .filter_map(VolumeRecord::mount)
            .collect();
        if let Some(other) = mounts
            .iter()
            .find(|m| m.mount_path.trim_end_matches('/') == mount_path.trim_end_matches('/'))
        {
            return Err(VolumeStoreError::MountPathTaken {
                owner: owner.to_string(),
                mount_path: mount_path.to_string(),
                other: other.volume.clone(),
            });
        }
        mounts.push(mount);
        let lent_volumes: Vec<Volume> = all.iter().map(|v| v.as_volume(lent)).collect();
        validate_mounts(lent, &mounts, &lent_volumes, "docker", &DOCKER_VOLUME_KINDS).map_err(
            |source| VolumeStoreError::Invalid {
                owner: owner.to_string(),
                name: volume.name.clone(),
                source: Box::new(source),
            },
        )?;
        let now = Utc::now();
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO workload_volume_attachments (volume_id, mount_path, read_only) \
                 VALUES ($1, $2, $3) \
                 ON CONFLICT (volume_id) DO UPDATE \
                 SET mount_path = EXCLUDED.mount_path, read_only = EXCLUDED.read_only, \
                     updated_at = $4",
                vec![
                    volume_id.into(),
                    mount_path.into(),
                    read_only.into(),
                    now.into(),
                ],
            ))
            .await
            .map_err(db("attaching the volume", owner))?;
        self.get(owner, volume_id).await
    }

    /// Stop mounting volume `volume_id`. The data stays. Idempotent.
    pub async fn detach(
        &self,
        owner: &Owner,
        volume_id: i64,
    ) -> Result<VolumeRecord, VolumeStoreError> {
        self.get(owner, volume_id).await?;
        attachments::Entity::delete_many()
            .filter(attachments::Column::VolumeId.eq(volume_id))
            .exec(self.db.as_ref())
            .await
            .map_err(db("detaching the volume", owner))?;
        self.get(owner, volume_id).await
    }

    /// Record that the volume's data now lives on `node_id`, if it had no
    /// node yet. Returns the node it is on afterwards: a concurrent first
    /// create elsewhere wins and is returned, never overwritten.
    pub async fn bind_node(
        &self,
        owner: &Owner,
        volume_id: i64,
        node_id: i32,
    ) -> Result<i32, VolumeStoreError> {
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE workload_volumes SET node_id = $1, updated_at = NOW() \
                 WHERE id = $2 AND node_id IS NULL AND deleted_at IS NULL",
                vec![node_id.into(), volume_id.into()],
            ))
            .await
            .map_err(db("binding the volume to its node", owner))?;
        let volume = self.get(owner, volume_id).await?;
        volume.node_id.ok_or(VolumeStoreError::Corrupt {
            volume_id,
            field: "node_id",
            reason: "still unset after binding".to_string(),
        })
    }

    /// Delete volume `volume_id`: the row is soft-deleted (its name is free
    /// again) and its attachment removed. The caller removes the backing
    /// when [`DeletedVolume::backing`] says so.
    pub async fn delete(
        &self,
        owner: &Owner,
        volume_id: i64,
    ) -> Result<DeletedVolume, VolumeStoreError> {
        let volume = self.get(owner, volume_id).await?;
        let txn = self
            .db
            .begin()
            .await
            .map_err(db("starting the volume delete", owner))?;
        attachments::Entity::delete_many()
            .filter(attachments::Column::VolumeId.eq(volume_id))
            .exec(&txn)
            .await
            .map_err(db("removing the deleted volume's attachment", owner))?;
        volumes::Entity::update_many()
            .col_expr(volumes::Column::DeletedAt, Expr::value(Utc::now()))
            .col_expr(volumes::Column::UpdatedAt, Expr::value(Utc::now()))
            .filter(volumes::Column::Id.eq(volume_id))
            .exec(&txn)
            .await
            .map_err(db("marking the volume deleted", owner))?;
        txn.commit()
            .await
            .map_err(db("committing the volume delete", owner))?;
        let backing = volume_disposition(&volume);
        tracing::info!(
            volume_id,
            owner = %owner,
            name = %volume.name,
            retained = backing == OnOwnerDelete::Retain,
            "volume deleted"
        );
        Ok(DeletedVolume { volume, backing })
    }

    /// Delete every volume of `owner` (the owner is going away).
    pub async fn delete_owned(
        &self,
        owner: &Owner,
    ) -> Result<Vec<DeletedVolume>, VolumeStoreError> {
        let mut deleted = Vec::new();
        for volume in self.all_of(owner).await? {
            deleted.push(self.delete(owner, volume.id).await?);
        }
        Ok(deleted)
    }
}

/// The volume kinds of the Docker driver, the only one deployments use.
pub const DOCKER_VOLUME_KINDS: VolumeKinds = VolumeKinds {
    named: true,
    block_image: false,
    preopened_dir: false,
};

/// Check `mounts` against the owner's `records` and say where they pin a
/// workload of `owner` (§8): `None` when no mounted volume has a node yet.
/// Pure; what the deploy job, the explanation and the plane all use.
pub fn pin_for(
    owner: WorkloadRef,
    mounts: &[VolumeMount],
    records: &[VolumeRecord],
    required_nodes: Option<&[i32]>,
) -> Result<Option<VolumePin>, VolumeError> {
    let lent: Vec<Volume> = records.iter().map(|r| r.as_volume(owner)).collect();
    let resolved = validate_mounts(owner, mounts, &lent, "docker", &DOCKER_VOLUME_KINDS)?;
    pinning(owner, &resolved, required_nodes)
}

/// What deleting `volume` does to its backing.
pub fn volume_disposition(volume: &VolumeRecord) -> OnOwnerDelete {
    on_owner_delete(&volume.as_volume(WorkloadRef::new(
        temps_workload::WorkloadKind::Deployment,
        0,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::test_db;

    fn environment(id: i64) -> Owner {
        Owner::new("environment", id)
    }

    fn new_volume(owner: &Owner, name: &str) -> NewVolume {
        NewVolume {
            owner: owner.clone(),
            name: name.to_string(),
            backing: BackingKind::Named,
            node_id: None,
            size_mb: Some(512),
            retain_on_delete: false,
        }
    }

    #[test]
    fn named_volumes_get_a_deterministic_docker_name() {
        assert_eq!(
            named_backing_ref(&environment(3), "data"),
            "temps-vol-environment-3-data"
        );
    }

    #[test]
    fn retain_on_delete_keeps_the_backing() {
        let mut record = VolumeRecord {
            id: 1,
            owner: environment(3),
            name: "data".into(),
            backing: Backing::Named {
                name: "temps-vol-environment-3-data".into(),
            },
            node_id: Some(0),
            size_mb: None,
            retain_on_delete: false,
            attachment: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        assert_eq!(volume_disposition(&record), OnOwnerDelete::Delete);
        record.retain_on_delete = true;
        assert_eq!(volume_disposition(&record), OnOwnerDelete::Retain);
    }

    #[test]
    fn a_mount_carries_the_resolved_bind_source() {
        let record = VolumeRecord {
            id: 1,
            owner: environment(3),
            name: "data".into(),
            backing: Backing::Named {
                name: "temps-vol-environment-3-data".into(),
            },
            node_id: None,
            size_mb: None,
            retain_on_delete: false,
            attachment: Some(VolumeAttachment {
                mount_path: "/var/lib/app".into(),
                read_only: true,
            }),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let mount = record.mount().expect("attached");
        assert_eq!(mount.volume, "data");
        assert_eq!(
            mount.source.as_deref(),
            Some("temps-vol-environment-3-data")
        );
        assert!(mount.read_only);
    }

    #[tokio::test]
    async fn volume_rows_round_trip_and_names_are_unique_per_live_owner() {
        let Some(test_db) = test_db().await else {
            return;
        };
        let store = VolumeStore::new(test_db.connection_arc());
        let env = environment(3);
        let created = store
            .create(new_volume(&env, "data"))
            .await
            .expect("created");
        assert_eq!(created.owner, env);
        assert_eq!(
            created.backing,
            Backing::Named {
                name: "temps-vol-environment-3-data".into()
            }
        );
        assert_eq!(created.size_mb, Some(512));
        assert_eq!(created.node_id, None);

        let taken = store.create(new_volume(&env, "data")).await.unwrap_err();
        assert!(
            matches!(taken, VolumeStoreError::NameTaken { ref name, .. } if name == "data"),
            "{taken}"
        );
        // Another owner may reuse the name.
        store
            .create(new_volume(&environment(4), "data"))
            .await
            .expect("other owner");

        let bad = store.create(new_volume(&env, "Data!")).await.unwrap_err();
        assert!(matches!(bad, VolumeStoreError::Invalid { .. }), "{bad}");

        let attached = store
            .attach(&env, created.id, "/var/lib/app", false)
            .await
            .expect("attached");
        assert_eq!(
            attached.attachment,
            Some(VolumeAttachment {
                mount_path: "/var/lib/app".into(),
                read_only: false
            })
        );
        let mounts = store.mounts_of(&env).await.expect("mounts");
        assert_eq!(mounts.len(), 1);
        assert_eq!(
            mounts[0].source.as_deref(),
            Some("temps-vol-environment-3-data")
        );

        // A second volume cannot take the same path, nor a reserved one.
        let logs = store.create(new_volume(&env, "logs")).await.expect("logs");
        let clash = store
            .attach(&env, logs.id, "/var/lib/app/", false)
            .await
            .unwrap_err();
        assert!(
            matches!(clash, VolumeStoreError::MountPathTaken { ref other, .. } if other == "data"),
            "{clash}"
        );
        let reserved = store
            .attach(&env, logs.id, "/run/secrets", false)
            .await
            .unwrap_err();
        assert!(reserved.to_string().contains("/run/secrets"), "{reserved}");

        // First create binds the node; a later bind elsewhere does not move it.
        assert_eq!(
            store.bind_node(&env, created.id, 0).await.expect("bound"),
            0
        );
        assert_eq!(store.bind_node(&env, created.id, 7).await.expect("kept"), 0);

        let (page, total) = store.list(&env, None, None).await.expect("listed");
        assert_eq!(total, 2);
        assert_eq!(page[0].name, "logs", "newest first");

        let missing = store.get(&environment(4), created.id).await.unwrap_err();
        assert!(
            matches!(missing, VolumeStoreError::NotFound { volume_id, .. } if volume_id == created.id),
            "another owner's volume is not found: {missing}"
        );

        let deleted = store.delete(&env, created.id).await.expect("deleted");
        assert_eq!(deleted.backing, OnOwnerDelete::Delete);
        assert!(store.mounts_of(&env).await.expect("mounts").is_empty());
        // The name is free again once deleted.
        store
            .create(new_volume(&env, "data"))
            .await
            .expect("reused");

        let mut kept = new_volume(&env, "keep");
        kept.retain_on_delete = true;
        store.create(kept).await.expect("kept");
        let owned = store.delete_owned(&env).await.expect("owner deleted");
        assert_eq!(owned.len(), 3);
        assert_eq!(
            owned
                .iter()
                .filter(|d| d.backing == OnOwnerDelete::Retain)
                .map(|d| d.volume.name.as_str())
                .collect::<Vec<_>>(),
            vec!["keep"]
        );
    }
}
