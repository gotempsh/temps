// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Which image tags and IDs a project still names.
//!
//! An image a project built can be named by another project too: a
//! promotion or rollback that deploys it, an image deployed by ID, or an
//! identical build that produced the same image ID under its own tag. Both
//! the deletion-time cleanup and the nightly orphan sweep must keep such an
//! image, so they ask the same question here, in one query for the whole
//! set of candidates: deployments' `image_name`, and the tags and IDs every
//! `BuildImageJob` recorded, including each platform of a multi-platform
//! build.

use std::collections::HashSet;

use sea_orm::{ConnectionTrait, DatabaseBackend, DbErr, Statement};

/// The image ID in `sha256:<lowercase hex>` form, and also as bare hex,
/// so a row that recorded either spelling matches.
pub fn image_id_spellings(image_id: &str) -> [String; 2] {
    let lower = image_id.trim().to_ascii_lowercase();
    let hex = lower.trim_start_matches("sha256:").to_string();
    [format!("sha256:{hex}"), hex]
}

/// A tag as recorded and with its implicit `:latest` made explicit or
/// dropped, so a deployment that names `app` matches the image `app:latest`.
pub fn tag_spellings(tag: &str) -> Vec<String> {
    let tag = tag.trim();
    let name = tag.rsplit('/').next().unwrap_or(tag);
    if tag.contains('@') {
        vec![tag.to_string()]
    } else if let Some(bare) = tag.strip_suffix(":latest") {
        vec![tag.to_string(), bare.to_string()]
    } else if name.contains(':') {
        vec![tag.to_string()]
    } else {
        vec![tag.to_string(), format!("{tag}:latest")]
    }
}

/// Whether `named` (from [`referenced_images`]) holds any spelling of `tag`.
pub fn names_tag(named: &HashSet<String>, tag: &str) -> bool {
    tag_spellings(tag)
        .iter()
        .any(|spelling| named.contains(spelling))
}

/// Whether `named` holds any spelling of `image_id`.
pub fn names_image_id(named: &HashSet<String>, image_id: &str) -> bool {
    image_id_spellings(image_id)
        .iter()
        .any(|spelling| named.contains(spelling))
}

/// The members of `candidates` (tags or image IDs, as recorded) that a
/// deployment or build of any project other than `excluding_project` names.
/// `None` checks every project, for images whose own project is gone.
pub async fn referenced_images<C: ConnectionTrait>(
    db: &C,
    candidates: &[String],
    excluding_project: Option<i32>,
) -> Result<HashSet<String>, DbErr> {
    if candidates.is_empty() {
        return Ok(HashSet::new());
    }
    let rows = db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            WITH builds AS (
                SELECT dj.outputs
                FROM deployment_jobs dj
                JOIN deployments d ON d.id = dj.deployment_id
                WHERE dj.job_type = 'BuildImageJob'
                  AND ($2::int IS NULL OR d.project_id <> $2)
            )
            SELECT DISTINCT reference FROM (
                SELECT d.image_name AS reference
                FROM deployments d
                WHERE ($2::int IS NULL OR d.project_id <> $2)
                  AND d.image_name = ANY($1::text[])
                UNION ALL
                SELECT b.outputs ->> 'image_tag' FROM builds b
                WHERE b.outputs ->> 'image_tag' = ANY($1::text[])
                UNION ALL
                SELECT b.outputs ->> 'image_id' FROM builds b
                WHERE b.outputs ->> 'image_id' = ANY($1::text[])
                UNION ALL
                SELECT per_platform.value
                FROM builds b
                CROSS JOIN LATERAL json_each_text(
                    CASE WHEN json_typeof(b.outputs -> 'image_tags_by_platform') = 'object'
                         THEN b.outputs -> 'image_tags_by_platform' ELSE '{}'::json END
                ) AS per_platform
                WHERE per_platform.value = ANY($1::text[])
                UNION ALL
                SELECT per_platform.value
                FROM builds b
                CROSS JOIN LATERAL json_each_text(
                    CASE WHEN json_typeof(b.outputs -> 'image_ids_by_platform') = 'object'
                         THEN b.outputs -> 'image_ids_by_platform' ELSE '{}'::json END
                ) AS per_platform
                WHERE per_platform.value = ANY($1::text[])
            ) refs
            WHERE reference IS NOT NULL
            "#,
            [candidates.to_vec().into(), excluding_project.into()],
        ))
        .await?;
    rows.into_iter()
        .map(|row| row.try_get::<String>("", "reference"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_image_id_matches_with_or_without_its_prefix() {
        assert_eq!(
            image_id_spellings("SHA256:ABcd"),
            ["sha256:abcd".to_string(), "abcd".to_string()]
        );
        assert_eq!(
            image_id_spellings("abcd"),
            ["sha256:abcd".to_string(), "abcd".to_string()]
        );
    }

    #[test]
    fn an_implicit_latest_tag_matches_either_spelling() {
        assert_eq!(tag_spellings("app-1"), vec!["app-1", "app-1:latest"]);
        assert_eq!(tag_spellings("app-1:latest"), vec!["app-1:latest", "app-1"]);
        assert_eq!(tag_spellings("app-1:v2"), vec!["app-1:v2"]);
        assert_eq!(
            tag_spellings("registry:5000/app"),
            vec!["registry:5000/app", "registry:5000/app:latest"]
        );
        let named = HashSet::from(["app-1".to_string()]);
        assert!(names_tag(&named, "app-1:latest"));
        assert!(!names_tag(&named, "app-1:v2"));
    }

    #[tokio::test]
    async fn no_candidates_asks_nothing() {
        // A mock with no results would fail any query.
        let db = sea_orm::MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        assert!(referenced_images(&db, &[], None)
            .await
            .expect("no query")
            .is_empty());
    }

    /// Against a real database: another project's deployment, its build's
    /// primary tag and ID, and a per-platform tag all count; the excluded
    /// project's own references do not. Skips without Postgres.
    #[tokio::test]
    async fn references_by_any_other_project_are_found() -> Result<(), Box<dyn std::error::Error>> {
        use sea_orm::{ActiveModelTrait, ActiveValue::Set};
        use temps_entities::{
            deployment_jobs, deployments, environments, projects, types::JobStatus,
        };

        let test_db = match temps_test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) => {
                eprintln!("Database unavailable; skipping image reference test: {error}");
                return Ok(());
            }
        };
        let db = test_db.connection_arc();
        let now = chrono::Utc::now();
        let mut ids = Vec::new();
        for slug in ["owner", "other"] {
            let project = projects::ActiveModel {
                name: Set(slug.to_string()),
                slug: Set(slug.to_string()),
                repo_owner: Set("o".to_string()),
                repo_name: Set("r".to_string()),
                preset: Set(temps_entities::preset::Preset::Dockerfile),
                directory: Set("/".to_string()),
                main_branch: Set("main".to_string()),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            }
            .insert(db.as_ref())
            .await?;
            let environment = environments::ActiveModel {
                project_id: Set(project.id),
                name: Set("Production".to_string()),
                slug: Set("production".to_string()),
                host: Set(format!("{slug}.example.test")),
                upstreams: Set(temps_entities::upstream_config::UpstreamList::default()),
                subdomain: Set(format!("{slug}.example.test")),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            }
            .insert(db.as_ref())
            .await?;
            ids.push((project.id, environment.id));
        }
        let deploy = |project: (i32, i32), slug: &str, image: &str| deployments::ActiveModel {
            project_id: Set(project.0),
            environment_id: Set(project.1),
            slug: Set(slug.to_string()),
            state: Set("deployed".to_string()),
            image_name: Set(Some(image.to_string())),
            metadata: Set(Some(deployments::DeploymentMetadata::default())),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        let own = deploy(ids[0], "owner-1", "owner-1:latest")
            .insert(db.as_ref())
            .await?;
        deployment_jobs::ActiveModel {
            deployment_id: Set(own.id),
            job_id: Set("build_image".to_string()),
            job_type: Set("BuildImageJob".to_string()),
            name: Set("Build".to_string()),
            log_id: Set("own".to_string()),
            status: Set(JobStatus::Success),
            outputs: Set(Some(serde_json::json!({
                "image_tag": "owner-1:latest", "image_id": "sha256:aa"
            }))),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;
        // The other project deploys a promoted tag, and its multi-platform
        // build produced the same image ID and a per-platform tag.
        let other = deploy(ids[1], "other-1", "promoted:latest")
            .insert(db.as_ref())
            .await?;
        deployment_jobs::ActiveModel {
            deployment_id: Set(other.id),
            job_id: Set("build_image".to_string()),
            job_type: Set("BuildImageJob".to_string()),
            name: Set("Build".to_string()),
            log_id: Set("other".to_string()),
            status: Set(JobStatus::Success),
            outputs: Set(Some(serde_json::json!({
                "image_tag": "other-1:latest",
                "image_id": "sha256:bb",
                "image_tags_by_platform": { "linux/arm64": "other-1:latest-arm64" },
                "image_ids_by_platform": { "linux/arm64": "sha256:cc" }
            }))),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        let candidates: Vec<String> = [
            "owner-1:latest",
            "sha256:aa",
            "promoted:latest",
            "sha256:bb",
            "other-1:latest-arm64",
            "sha256:cc",
            "nobody:latest",
        ]
        .map(str::to_string)
        .to_vec();
        let found = referenced_images(db.as_ref(), &candidates, Some(ids[0].0)).await?;
        let expected: HashSet<String> = [
            "promoted:latest",
            "sha256:bb",
            "other-1:latest-arm64",
            "sha256:cc",
        ]
        .map(str::to_string)
        .into();
        assert_eq!(found, expected);

        let everyone = referenced_images(db.as_ref(), &candidates, None).await?;
        assert!(everyone.contains("owner-1:latest") && everyone.contains("sha256:aa"));
        assert!(!everyone.contains("nobody:latest"));
        Ok(())
    }
}
