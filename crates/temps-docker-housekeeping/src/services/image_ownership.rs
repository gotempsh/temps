// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Which images this install built, and for which project and deployment.
//!
//! Every image the deployment pipeline builds carries three labels under
//! `sh.temps.image.`: this install's ID, the project and the deployment. The
//! project and deployment IDs are local to one database; the install ID is
//! what makes them meaningful on a Docker host another Temps install may
//! share. Housekeeping only ever acts on images labelled with this install's
//! ID (see `orphaned_images`), so another install's images are never touched.
//!
//! The labels are their own namespace, not `sh.temps.*` at large: a container
//! inherits its image's labels, and a container's `sh.temps.project_id` is
//! Temps' container configuration, which an image must never set.

use std::collections::HashMap;

use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

/// The install that built the image (`image_ownership_identity.instance_id`).
pub const IMAGE_INSTANCE_LABEL: &str = "sh.temps.image.instance";
/// The project the image was built for, in the building install's database.
pub const IMAGE_PROJECT_LABEL: &str = "sh.temps.image.project_id";
/// The deployment whose build produced the image.
pub const IMAGE_DEPLOYMENT_LABEL: &str = "sh.temps.image.deployment_id";

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
/// install `instance_id`.
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
    fn a_missing_or_malformed_project_label_names_no_project() {
        let mut labels = HashMap::new();
        assert_eq!(labelled_project_id(&labels), None);
        labels.insert(IMAGE_PROJECT_LABEL.to_string(), "not-a-number".to_string());
        assert_eq!(labelled_project_id(&labels), None);
        labels.insert(IMAGE_PROJECT_LABEL.to_string(), "0".to_string());
        assert_eq!(labelled_project_id(&labels), None);
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
