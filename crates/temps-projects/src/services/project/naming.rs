// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Project display-name validation, slug generation and renames.

use super::*;

/// Normalize and validate a project display name.
///
/// The name is not just a label: it becomes `OTEL_SERVICE_NAME` on the next
/// deployment, and the Compose env-file renderer rejects control characters —
/// so a name accepted here but refused there turns one bad save into every
/// later deploy failing with an error naming a variable the operator never set.
/// Format characters (bidi overrides, zero-width spaces) are rejected too:
/// names are not unique and are rendered next to each other in the dashboard,
/// alerts, and audit records, where an override can make one project display as
/// another.
///
/// Shared by every path that writes `projects.name` so the three cannot drift.
pub(super) fn validate_project_name(raw: &str) -> Result<String, ProjectError> {
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() || trimmed.chars().count() > 100 {
        return Err(ProjectError::InvalidInput(
            "Project name must contain 1-100 characters".to_string(),
        ));
    }
    if trimmed.chars().any(|c| {
        c.is_control()
            || matches!(c,
                '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2066}'..='\u{2069}'
                | '\u{FEFF}')
    }) {
        return Err(ProjectError::InvalidInput(
            "Project name cannot contain control or text-direction characters".to_string(),
        ));
    }
    Ok(trimmed)
}

pub(super) fn slugify(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn base_project_slug(name: &str) -> String {
    let mut slug = slugify(name);
    // Leave room for the production environment and service namespace within
    // a single DNS label. This must stay identical for planning and creation.
    if slug.chars().count() > 40 {
        slug = slug.chars().take(40).collect::<String>();
        slug.truncate(slug.trim_end_matches('-').len());
    }
    slug
}

impl ProjectService {
    /// Take the row lock and report the rename `new_name` would perform against
    /// the currently-stored name, or `None` when it matches and no write is
    /// needed.
    ///
    /// Both ends of the transition come from the locked read, so the pair always
    /// describes one real change. Callers must perform the write on the same
    /// `conn`: reading here and writing outside this transaction would let a
    /// concurrent rename land in between and make the reported transition
    /// fiction.
    /// Takes a transaction rather than a generic connection on purpose: passing
    /// an autocommit handle would compile and silently discard the `FOR UPDATE`
    /// guarantee this whole design rests on, so the type makes that unrepresentable.
    pub(super) async fn locked_rename(
        &self,
        txn: &DatabaseTransaction,
        project_id: i32,
        new_name: &str,
    ) -> Result<Option<ProjectRename>, ProjectError> {
        let locked = projects::Entity::find_by_id(project_id)
            .lock_exclusive()
            .one(txn)
            .await?
            .ok_or(ProjectError::NotFound(format!(
                "Project {} not found",
                project_id
            )))?;

        if locked.name.to_lowercase() != new_name.to_lowercase() {
            self.lock_project_name_available(txn, new_name, Some(project_id))
                .await?;
        }
        Ok(if locked.name == new_name {
            None
        } else {
            Some(ProjectRename {
                from: locked.name,
                to: new_name.to_string(),
            })
        })
    }

    /// Refuse a display name another active project already uses.
    ///
    /// Project names are not unique in the schema -- the slug is the
    /// identifier -- but the name is what the console, CLI, notifications and
    /// audit log show, so two active projects with the same name are
    /// indistinguishable everywhere except their URL. A duplicate name used to
    /// be accepted silently and given a random slug suffix (`whoami-3f9a1c`),
    /// which is exactly that confusion. The comparison ignores case, since
    /// `Whoami` and `whoami` read as the same project too.
    ///
    /// Deleted projects are ignored: their slug stays reserved (handled by
    /// [`Self::generate_unique_project_slug`]), but their name is free to reuse.
    /// `exclude_project_id` lets a project keep -- or re-case -- its own name.
    /// Serialize create/rename writes by normalized display name. Existing
    /// installations need no data-changing uniqueness migration.
    pub(super) async fn persist_named_project(
        &self,
        active: projects::ActiveModel,
        inserting: bool,
    ) -> Result<projects::Model, ProjectError> {
        let name =
            active.name.clone().take().ok_or_else(|| {
                ProjectError::InvalidInput("Project name is required".to_string())
            })?;
        let id = active.id.clone().take();
        let slug = active.slug.clone().take().unwrap_or_default();
        let txn = self.db.begin().await?;
        let changing_name = if !inserting {
            let current = projects::Entity::find_by_id(id.ok_or_else(|| {
                ProjectError::InvalidInput("Project ID is required for an update".to_string())
            })?)
            .lock_exclusive()
            .one(&txn)
            .await?
            .ok_or_else(|| {
                ProjectError::NotFound("Project disappeared during update".to_string())
            })?;
            current.name.to_lowercase() != name.to_lowercase()
        } else {
            true
        };
        // Legacy duplicate display names must not block unrelated edits or
        // recasing. Only a new normalized name claims the shared namespace.
        if changing_name {
            if let Err(error) = self
                .lock_project_name_available(&txn, &name, if inserting { None } else { id })
                .await
            {
                txn.rollback().await?;
                return Err(error);
            }
        }
        let result = if inserting {
            active.insert(&txn).await
        } else {
            active.update(&txn).await
        };
        let result = result.map_err(|e| {
            if super::types::is_unique_violation(&e) {
                ProjectError::SlugConflict { slug }
            } else {
                ProjectError::DatabaseError {
                    reason: e.to_string(),
                }
            }
        })?;
        txn.commit().await?;
        Ok(result)
    }

    async fn lock_project_name_available(
        &self,
        txn: &DatabaseTransaction,
        name: &str,
        exclude: Option<i32>,
    ) -> Result<(), ProjectError> {
        use sea_orm::sea_query::{Expr, Func};
        txn.query_one(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            [format!("temps-project-name:{}", name.to_lowercase()).into()],
        ))
        .await?;
        let mut query = projects::Entity::find()
            .filter(projects::Column::IsDeleted.eq(false))
            .filter(
                Expr::expr(Func::lower(Expr::col(projects::Column::Name))).eq(name.to_lowercase()),
            );
        if let Some(id) = exclude {
            query = query.filter(projects::Column::Id.ne(id));
        }
        if query.one(txn).await?.is_some() {
            return Err(ProjectError::NameAlreadyExists {
                name: name.to_string(),
            });
        }
        Ok(())
    }

    pub(super) async fn ensure_project_name_available(
        &self,
        name: &str,
        exclude_project_id: Option<i32>,
    ) -> Result<(), ProjectError> {
        use sea_orm::sea_query::{Expr, Func};

        let mut query = projects::Entity::find()
            .filter(projects::Column::IsDeleted.eq(false))
            .filter(
                Expr::expr(Func::lower(Expr::col(projects::Column::Name))).eq(name.to_lowercase()),
            );
        if let Some(project_id) = exclude_project_id {
            query = query.filter(projects::Column::Id.ne(project_id));
        }
        match query.one(self.db.as_ref()).await? {
            Some(_) => Err(ProjectError::NameAlreadyExists {
                name: name.to_string(),
            }),
            None => Ok(()),
        }
    }

    /// Generate a unique project slug by checking for collisions and appending a short UUID if needed.
    /// Slug is truncated to 40 chars max to keep DNS labels within the 63-char limit
    /// when combined with environment slug and service name prefix.
    pub async fn generate_unique_project_slug(&self, name: &str) -> Result<String, ProjectError> {
        let base_slug = base_project_slug(name);

        // First, try the base slug
        let existing = projects::Entity::find()
            .filter(projects::Column::Slug.eq(&base_slug))
            .one(self.db.as_ref())
            .await
            .map_err(|e| ProjectError::Other(e.to_string()))?;

        if existing.is_none() {
            return Ok(base_slug);
        }

        // If base slug exists, generate a short UUID suffix
        let short_uuid = Uuid::new_v4()
            .to_string()
            .chars()
            .filter(|c| c.is_alphanumeric())
            .take(6)
            .collect::<String>()
            .to_lowercase();

        let unique_slug = format!("{}-{}", base_slug, short_uuid);

        // Double check that this unique slug doesn't exist (extremely unlikely but be safe)
        let existing_unique = projects::Entity::find()
            .filter(projects::Column::Slug.eq(&unique_slug))
            .one(self.db.as_ref())
            .await
            .map_err(|e| ProjectError::Other(e.to_string()))?;

        if existing_unique.is_some() {
            // This is extremely unlikely, but if it happens, generate a new UUID
            let retry_uuid = Uuid::new_v4()
                .to_string()
                .chars()
                .filter(|c| c.is_alphanumeric())
                .take(8)
                .collect::<String>()
                .to_lowercase();
            Ok(format!("{}-{}", base_slug, retry_uuid))
        } else {
            Ok(unique_slug)
        }
    }

    /// Validate a new project's name and choose its slug, before any side
    /// effect (a forked repository, provisioned services) of a multi-step
    /// create. Refuses a name another active project uses, so a template
    /// deploy fails here rather than after creating resources.
    pub async fn plan_project_slug(&self, name: &str) -> Result<String, ProjectError> {
        let validated_name = validate_project_name(name)?;
        self.ensure_project_name_available(&validated_name, None)
            .await?;
        self.generate_unique_project_slug(&validated_name).await
    }

    pub(super) async fn validate_expected_project_slug(
        &self,
        name: &str,
        expected_slug: &str,
    ) -> Result<(), ProjectError> {
        let base_slug = base_project_slug(name);
        let suffix = expected_slug
            .strip_prefix(&format!("{base_slug}-"))
            .filter(|suffix| matches!(suffix.len(), 6 | 8))
            .filter(|suffix| suffix.bytes().all(|byte| byte.is_ascii_alphanumeric()));
        if expected_slug != base_slug && suffix.is_none() {
            return Err(ProjectError::InvalidInput(format!(
                "Expected slug '{expected_slug}' does not match project name '{name}'"
            )));
        }
        let exists = projects::Entity::find()
            .filter(projects::Column::Slug.eq(expected_slug))
            .one(self.db.as_ref())
            .await
            .map_err(|error| ProjectError::DatabaseError {
                reason: format!(
                    "failed to validate expected project slug '{expected_slug}': {error}"
                ),
            })?
            .is_some();
        if exists {
            return Err(ProjectError::SlugConflict {
                slug: expected_slug.to_string(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planned_slug_base_matches_creation_truncation() {
        let slug = base_project_slug(
            "This project name is deliberately much longer than the DNS allocation permits",
        );
        assert!(slug.len() <= 40);
        assert!(slug.starts_with("this-project-name-is-deliberately"));
    }

    #[test]
    fn planned_slug_truncation_is_safe_for_unicode_names() {
        let slug = base_project_slug(&"é".repeat(50));
        assert_eq!(slug.chars().count(), 40);
    }
}
