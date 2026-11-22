// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Which images this install built, and for which project and deployment.
//!
//! Every image the deployment pipeline builds carries labels under
//! `sh.temps.image.`: this install's owner ID, the project, the deployment
//! and the tag the build gave it. The project and deployment IDs are local
//! to one database; the owner ID is what makes them meaningful on a Docker
//! host another Temps install may share. Housekeeping only ever acts on
//! images labelled with this install's owner ID (see `orphaned_images`), so
//! another install's images are never touched.
//!
//! The owner ID is `<database ID>.<data directory ID>`:
//!
//! - the **database ID** (`image_ownership_identity`) survives a restart, an
//!   upgrade and a move of the install;
//! - the **data directory ID** (a file in `TEMPS_DATA_DIR`) is not copied
//!   when a database is restored into another install, so a staging install
//!   started from production's database on the same Docker host gets a
//!   different owner ID and neither sweep can remove the other's images.
//!
//! Losing either half (a new data directory, a database restored without
//! the row) only makes earlier images unrecognisable: they are then left on
//! disk, never removed wrongly.
//!
//! The labels are their own namespace, not `sh.temps.*` at large: a container
//! inherits its image's labels, and a container's `sh.temps.project_id` is
//! Temps' container configuration, which an image must never set.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

/// The file in the install's data directory holding its half of the owner
/// ID.
pub const INSTALL_MARKER_FILE: &str = "image-ownership.id";

/// The install that built the image (`image_ownership_identity.instance_id`).
pub const IMAGE_INSTANCE_LABEL: &str = "sh.temps.image.instance";
/// The project the image was built for, in the building install's database.
pub const IMAGE_PROJECT_LABEL: &str = "sh.temps.image.project_id";
/// The deployment whose build produced the image.
pub const IMAGE_DEPLOYMENT_LABEL: &str = "sh.temps.image.deployment_id";
/// The tag the build gave the image. The sweep only removes an image whose
/// one tag is still this one: an image an operator retagged is theirs.
pub const IMAGE_TAG_LABEL: &str = "sh.temps.image.tag";

/// Why this install's image ownership ID could not be read or created.
#[derive(Debug, thiserror::Error)]
pub enum ImageOwnershipError {
    #[error("Failed to {operation} the image ownership instance ID (image_ownership_identity): {source}")]
    Database {
        operation: &'static str,
        #[source]
        source: sea_orm::DbErr,
    },

    #[error(
        "The image ownership instance ID could not be created: no row in image_ownership_identity after inserting one"
    )]
    Missing,

    #[error("Failed to {operation} the install's image ownership marker {path}: {source}")]
    Marker {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "The install's image ownership marker {path} does not hold an ID (expected 1-64 \
         hexadecimal digits or '-'). Removing it makes images built so far unrecognisable \
         to housekeeping (they are then kept, not removed)"
    )]
    MarkerInvalid { path: PathBuf },
}

fn is_valid_marker(marker: &str) -> bool {
    !marker.is_empty()
        && marker.len() <= 64
        && marker
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
}

/// This install's half of the owner ID, read from its data directory, and
/// created there first when `create` is set. `None` when it does not exist
/// and `create` is not set.
///
/// Creation is serialized by an exclusive lock on [`INSTALL_MARKER_LOCK_FILE`]
/// (an advisory file lock, so it covers every process using the data
/// directory, and every task of this one). Under the lock the marker is read
/// again and only published when there still is none (or only an empty one,
/// which no image can carry): every racing creator gets the one published
/// marker. It is written and synced to a private temporary file first and
/// then put in place, so a reader without the lock, or a crash at any point,
/// sees either no marker or a complete one.
pub async fn install_marker(
    data_dir: &Path,
    create: bool,
) -> Result<Option<String>, ImageOwnershipError> {
    let path = data_dir.join(INSTALL_MARKER_FILE);
    let found = match tokio::fs::read(&path).await {
        Ok(bytes) => parse_marker(&path, &bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => MarkerFile::Missing,
        Err(source) => {
            return Err(ImageOwnershipError::Marker {
                operation: "read",
                path,
                source,
            })
        }
    };
    match found {
        MarkerFile::Valid(marker) => Ok(Some(marker)),
        MarkerFile::Missing | MarkerFile::Empty if !create => Ok(None),
        MarkerFile::Missing | MarkerFile::Empty => {
            let data_dir = data_dir.to_path_buf();
            tokio::task::spawn_blocking(move || create_marker_locked(&data_dir))
                .await
                .map_err(|error| ImageOwnershipError::Marker {
                    operation: "create",
                    path,
                    source: std::io::Error::other(error.to_string()),
                })?
                .map(Some)
        }
    }
}

/// The file whose lock serializes creating the marker. Left in place.
pub const INSTALL_MARKER_LOCK_FILE: &str = "image-ownership.id.lock";

/// What the marker file holds.
enum MarkerFile {
    Missing,
    Empty,
    Valid(String),
}

fn parse_marker(path: &Path, bytes: &[u8]) -> Result<MarkerFile, ImageOwnershipError> {
    // A marker is a UUID; anything larger is not one.
    let invalid = || ImageOwnershipError::MarkerInvalid {
        path: path.to_path_buf(),
    };
    if bytes.len() > 256 {
        return Err(invalid());
    }
    let marker = std::str::from_utf8(bytes).map_err(|_| invalid())?.trim();
    if marker.is_empty() {
        Ok(MarkerFile::Empty)
    } else if is_valid_marker(marker) {
        Ok(MarkerFile::Valid(marker.to_string()))
    } else {
        Err(invalid())
    }
}

/// Create the marker under the creation lock, unless another creator did
/// first. Blocking: runs on the blocking pool.
fn create_marker_locked(data_dir: &Path) -> Result<String, ImageOwnershipError> {
    let path = data_dir.join(INSTALL_MARKER_FILE);
    let marker_error = |operation, source| ImageOwnershipError::Marker {
        operation,
        path: path.clone(),
        source,
    };
    std::fs::create_dir_all(data_dir)
        .map_err(|source| marker_error("create the directory of", source))?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(data_dir.join(INSTALL_MARKER_LOCK_FILE))
        .map_err(|source| marker_error("open the creation lock of", source))?;
    lock.lock()
        .map_err(|source| marker_error("lock the creation of", source))?;
    // Under the lock: another creator may have published meanwhile.
    let existing = match std::fs::read(&path) {
        Ok(bytes) => parse_marker(&path, &bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => MarkerFile::Missing,
        Err(source) => return Err(marker_error("read", source)),
    };
    if let MarkerFile::Valid(marker) = existing {
        return Ok(marker);
    }
    let marker = uuid::Uuid::new_v4().to_string();
    let staged = data_dir.join(format!(
        ".{INSTALL_MARKER_FILE}.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let written = (|| {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)
            .map_err(|source| marker_error("stage", source))?;
        file.write_all(marker.as_bytes())
            .map_err(|source| marker_error("write", source))?;
        file.sync_all()
            .map_err(|source| marker_error("sync", source))?;
        // Holding the lock, nobody else publishes: replacing a missing or
        // empty marker is safe either way, and rename puts it in place
        // whole.
        std::fs::rename(&staged, &path).map_err(|source| marker_error("publish", source))
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    written?;
    drop(lock);
    Ok(marker)
}

/// This install's owner ID for image labels, both halves created on first
/// use.
pub async fn ensure_image_owner_id<C: ConnectionTrait>(
    db: &C,
    data_dir: &Path,
) -> Result<String, ImageOwnershipError> {
    let database = ensure_image_instance_id(db).await?;
    let marker = install_marker(data_dir, true).await?.ok_or_else(|| {
        ImageOwnershipError::MarkerInvalid {
            path: data_dir.join(INSTALL_MARKER_FILE),
        }
    })?;
    Ok(format!("{database}.{marker}"))
}

/// This install's owner ID, or `None` if either half does not exist yet:
/// then no image can carry it, and there is nothing of ours to find.
pub async fn load_image_owner_id<C: ConnectionTrait>(
    db: &C,
    data_dir: &Path,
) -> Result<Option<String>, ImageOwnershipError> {
    let Some(database) = load_image_instance_id(db).await? else {
        return Ok(None);
    };
    Ok(install_marker(data_dir, false)
        .await?
        .map(|marker| format!("{database}.{marker}")))
}

/// This install's image ownership ID, or `None` if no build has created it
/// yet (then no image carries it, and there is nothing of ours to find).
pub async fn load_image_instance_id<C: ConnectionTrait>(
    db: &C,
) -> Result<Option<String>, ImageOwnershipError> {
    let row = db
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT instance_id FROM image_ownership_identity WHERE id = 1",
        ))
        .await
        .map_err(|source| ImageOwnershipError::Database {
            operation: "read",
            source,
        })?;
    row.map(|row| row.try_get::<String>("", "instance_id"))
        .transpose()
        .map_err(|source| ImageOwnershipError::Database {
            operation: "decode",
            source,
        })
}

/// This install's image ownership ID, created on first use. Never rotated:
/// two concurrent first builds both insert with `ON CONFLICT DO NOTHING` and
/// read back the single winner.
pub async fn ensure_image_instance_id<C: ConnectionTrait>(
    db: &C,
) -> Result<String, ImageOwnershipError> {
    if let Some(id) = load_image_instance_id(db).await? {
        return Ok(id);
    }
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO image_ownership_identity (id, instance_id) VALUES (1, $1) \
         ON CONFLICT (id) DO NOTHING",
        [uuid::Uuid::new_v4().to_string().into()],
    ))
    .await
    .map_err(|source| ImageOwnershipError::Database {
        operation: "create",
        source,
    })?;
    load_image_instance_id(db)
        .await?
        .ok_or(ImageOwnershipError::Missing)
}

/// The labels for an image built for `deployment_id` of `project_id` by the
/// install whose owner ID is `instance_id`.
pub fn image_ownership_labels(
    instance_id: &str,
    project_id: i32,
    deployment_id: i32,
) -> HashMap<String, String> {
    HashMap::from([
        (IMAGE_INSTANCE_LABEL.to_string(), instance_id.to_string()),
        (IMAGE_PROJECT_LABEL.to_string(), project_id.to_string()),
        (
            IMAGE_DEPLOYMENT_LABEL.to_string(),
            deployment_id.to_string(),
        ),
    ])
}

/// `labels` (the ownership labels of a build) for the image tagged `tag`.
/// Without ownership labels there is nothing to add: the image stays
/// unrecognised, as before.
pub fn labels_for_tag(labels: &HashMap<String, String>, tag: &str) -> HashMap<String, String> {
    let mut labels = labels.clone();
    if labels.contains_key(IMAGE_INSTANCE_LABEL) {
        labels.insert(IMAGE_TAG_LABEL.to_string(), tag.to_string());
    }
    labels
}

/// The project an image's labels name, if they name a valid one.
pub fn labelled_project_id(labels: &HashMap<String, String>) -> Option<i32> {
    labels
        .get(IMAGE_PROJECT_LABEL)
        .and_then(|value| value.trim().parse::<i32>().ok())
        .filter(|id| *id > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_name_the_install_project_and_deployment_in_their_own_namespace() {
        let labels = image_ownership_labels("install-a", 7, 42);
        assert_eq!(labels[IMAGE_INSTANCE_LABEL], "install-a");
        assert_eq!(labels[IMAGE_PROJECT_LABEL], "7");
        assert_eq!(labels[IMAGE_DEPLOYMENT_LABEL], "42");
        assert!(
            labels.keys().all(|key| key.starts_with("sh.temps.image.")),
            "image labels must not collide with container labels: {labels:?}"
        );
        assert_eq!(labelled_project_id(&labels), Some(7));
    }

    #[test]
    fn each_built_image_records_its_own_tag_only_when_it_is_owned() {
        let owned = labels_for_tag(&image_ownership_labels("install-a", 7, 42), "app-42:latest");
        assert_eq!(owned[IMAGE_TAG_LABEL], "app-42:latest");
        assert!(labels_for_tag(&HashMap::new(), "app-42:latest").is_empty());
    }

    #[test]
    fn a_missing_or_malformed_project_label_names_no_project() {
        let mut labels = HashMap::new();
        assert_eq!(labelled_project_id(&labels), None);
        labels.insert(IMAGE_PROJECT_LABEL.to_string(), "not-a-number".to_string());
        assert_eq!(labelled_project_id(&labels), None);
        labels.insert(IMAGE_PROJECT_LABEL.to_string(), "0".to_string());
        assert_eq!(labelled_project_id(&labels), None);
    }

    #[tokio::test]
    async fn the_install_marker_is_created_once_and_read_back() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert_eq!(install_marker(dir.path(), false).await.expect("read"), None);
        let created = install_marker(dir.path(), true)
            .await
            .expect("create")
            .expect("a marker");
        assert!(is_valid_marker(&created), "{created}");
        assert_eq!(
            install_marker(dir.path(), true).await.expect("read back"),
            Some(created.clone())
        );
        assert_eq!(
            install_marker(dir.path(), false).await.expect("read back"),
            Some(created)
        );
    }

    /// Racing creators all get the same, complete marker: none ever reads a
    /// half-created file.
    #[tokio::test]
    async fn concurrent_creators_all_get_the_one_published_marker() {
        let dir = tempfile::tempdir().expect("temp dir");
        let tasks: Vec<_> = (0..32)
            .map(|_| {
                let dir = dir.path().to_path_buf();
                tokio::spawn(async move { install_marker(&dir, true).await })
            })
            .collect();
        let mut markers = std::collections::HashSet::new();
        for task in tasks {
            let marker = task
                .await
                .expect("task")
                .expect("no creator sees a partial marker")
                .expect("a marker");
            markers.insert(marker);
        }
        assert_eq!(markers.len(), 1, "{markers:?}");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .expect("list")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name != INSTALL_MARKER_FILE && name != INSTALL_MARKER_LOCK_FILE)
            .collect();
        assert!(
            leftovers.is_empty(),
            "staged files are cleaned up: {leftovers:?}"
        );
    }

    /// Racing creators over an empty marker also all get one marker: the
    /// replacement happens once, under the creation lock.
    #[tokio::test]
    async fn concurrent_creators_replacing_an_empty_marker_agree() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join(INSTALL_MARKER_FILE), "").expect("write");
        let tasks: Vec<_> = (0..32)
            .map(|_| {
                let dir = dir.path().to_path_buf();
                tokio::spawn(async move { install_marker(&dir, true).await })
            })
            .collect();
        let mut markers = std::collections::HashSet::new();
        for task in tasks {
            markers.insert(
                task.await
                    .expect("task")
                    .expect("no error")
                    .expect("a marker"),
            );
        }
        assert_eq!(markers.len(), 1, "{markers:?}");
        let on_disk = std::fs::read_to_string(dir.path().join(INSTALL_MARKER_FILE)).expect("read");
        assert!(
            markers.contains(&on_disk),
            "every creator returned the marker on disk"
        );
    }

    /// An empty marker (a crash before it was written) holds no ID that any
    /// image could carry: it is replaced rather than failing every build.
    #[tokio::test]
    async fn an_empty_marker_left_by_a_crash_is_replaced() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(INSTALL_MARKER_FILE);
        std::fs::write(&path, "").expect("write");
        assert_eq!(install_marker(dir.path(), false).await.expect("read"), None);
        let marker = install_marker(dir.path(), true)
            .await
            .expect("replaced")
            .expect("a marker");
        assert!(is_valid_marker(&marker));
        assert_eq!(std::fs::read_to_string(&path).expect("read"), marker);
    }

    /// A damaged marker is never replaced silently: replacing it would make
    /// every image built so far unrecognisable without saying why.
    #[tokio::test]
    async fn a_damaged_marker_is_refused_not_replaced() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(INSTALL_MARKER_FILE);
        std::fs::write(&path, "not a marker!").expect("write");
        assert!(matches!(
            install_marker(dir.path(), true).await,
            Err(ImageOwnershipError::MarkerInvalid { .. })
        ));
        assert_eq!(
            std::fs::read_to_string(&path).expect("kept"),
            "not a marker!"
        );
    }

    /// Against a real database: a database copied into a second install
    /// (another data directory) yields another owner ID, while the same
    /// install always yields the same one. Skips without Postgres.
    #[tokio::test]
    async fn a_copied_database_in_another_install_gets_another_owner_id(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = match temps_test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) => {
                eprintln!("Database unavailable; skipping owner ID test: {error}");
                return Ok(());
            }
        };
        let db = test_db.connection_arc();
        let production = tempfile::tempdir()?;
        let staging = tempfile::tempdir()?;
        assert_eq!(
            load_image_owner_id(db.as_ref(), production.path()).await?,
            None
        );
        let original = ensure_image_owner_id(db.as_ref(), production.path()).await?;
        assert_eq!(
            ensure_image_owner_id(db.as_ref(), production.path()).await?,
            original
        );
        assert_eq!(
            load_image_owner_id(db.as_ref(), production.path()).await?,
            Some(original.clone())
        );
        // Same database row, another data directory: not the same owner.
        assert_eq!(
            load_image_owner_id(db.as_ref(), staging.path()).await?,
            None
        );
        let copy = ensure_image_owner_id(db.as_ref(), staging.path()).await?;
        assert_ne!(copy, original);
        assert_eq!(
            copy.split('.').next(),
            original.split('.').next(),
            "both halves are shown: {copy} / {original}"
        );
        Ok(())
    }

    /// Against a real database: the ID is absent until first use, created
    /// once, and stable on every later call. Skips without Docker/Postgres.
    #[tokio::test]
    async fn the_instance_id_is_created_once_and_never_rotates(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = match temps_test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) => {
                eprintln!("Database unavailable; skipping image ownership ID test: {error}");
                return Ok(());
            }
        };
        let db = test_db.connection_arc();
        assert_eq!(load_image_instance_id(db.as_ref()).await?, None);
        let first = ensure_image_instance_id(db.as_ref()).await?;
        let second = ensure_image_instance_id(db.as_ref()).await?;
        assert!(!first.is_empty());
        assert_eq!(first, second);
        assert_eq!(load_image_instance_id(db.as_ref()).await?, Some(first));
        Ok(())
    }
}
