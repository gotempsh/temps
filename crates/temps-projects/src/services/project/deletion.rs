// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Project deletion and the delivery-binding guard that precedes it.

use super::*;

impl ProjectService {
    /// Fail with [`ProjectError::DeliveryBindingsExist`] when the project
    /// still has CDN delivery bindings, naming the affected hostnames.
    pub async fn ensure_no_delivery_bindings(&self, project_id: i32) -> Result<(), ProjectError> {
        Self::refuse_delivery_bindings(self.db.as_ref(), project_id).await
    }

    /// [`Self::ensure_no_delivery_bindings`] on `connection`, so a caller
    /// holding the project row lock counts bindings inside its transaction.
    async fn refuse_delivery_bindings<C: ConnectionTrait>(
        connection: &C,
        project_id: i32,
    ) -> Result<(), ProjectError> {
        let bindings: Vec<(i32, String)> = domain_delivery_bindings::Entity::find()
            .filter(domain_delivery_bindings::Column::ProjectId.eq(project_id))
            .select_only()
            .column(domain_delivery_bindings::Column::Id)
            .column(domain_delivery_bindings::Column::Hostname)
            .order_by_asc(domain_delivery_bindings::Column::Hostname)
            .into_tuple()
            .all(connection)
            .await?;
        if bindings.is_empty() {
            return Ok(());
        }
        let (binding_ids, hostnames): (Vec<i32>, Vec<String>) = bindings.into_iter().unzip();
        Err(ProjectError::DeliveryBindingsExist {
            project_id,
            binding_count: hostnames.len(),
            hostnames,
            binding_ids,
        })
    }

    /// Persist deletion intent before cancelling workflows or touching Docker.
    /// Deployment workers reject projects with this fence, closing the window
    /// where a new container could appear after the cleanup snapshot.
    ///
    /// The project row is locked `FOR UPDATE`, delivery bindings are counted
    /// and the fence is written in one transaction. A domain delivery
    /// reservation share-locks the same row and refuses a fenced project, so
    /// either it commits first and its binding is seen here, or it waits for
    /// this fence and refuses. A binding can therefore never appear behind the
    /// fence, where it would block the final delete while its cleanup refuses
    /// the deleted project. Retrying an already-fenced project is a no-op.
    pub async fn begin_project_deletion(&self, project_id: i32) -> Result<(), ProjectError> {
        let txn = self.db.begin().await?;
        let outcome = Self::fence_locked_project(&txn, project_id).await;
        // End the transaction before returning, refusal included, so the row
        // lock is released when the caller sees the result rather than when
        // the pool gets round to rolling back a dropped transaction.
        match outcome {
            Ok(fenced) => {
                txn.commit().await?;
                if fenced {
                    info!(project_id, "Marked project for deletion");
                }
                Ok(())
            }
            Err(error) => {
                if let Err(rollback_error) = txn.rollback().await {
                    error!(
                        "Failed to roll back the deletion fence of project {} after it failed with '{}': {}",
                        project_id, error, rollback_error
                    );
                }
                Err(error)
            }
        }
    }

    /// The checks and the fence of [`Self::begin_project_deletion`], on
    /// `txn`. Returns whether this call wrote the fence: `false` when the
    /// project was already fenced.
    async fn fence_locked_project(
        txn: &DatabaseTransaction,
        project_id: i32,
    ) -> Result<bool, ProjectError> {
        let project = projects::Entity::find_by_id(project_id)
            .lock_exclusive()
            .one(txn)
            .await?
            .ok_or_else(|| ProjectError::NotFound(format!("project {} not found", project_id)))?;
        // Refuse before fencing or touching containers: a delivery binding
        // owns DNS records and CDN hostnames that only the delivery service
        // can clean up, and its foreign key would block the final delete.
        Self::refuse_delivery_bindings(txn, project_id).await?;
        if project.is_deleted {
            return Ok(false);
        }

        let mut active: projects::ActiveModel = project.into();
        active.is_deleted = Set(true);
        active.deleted_at = Set(Some(chrono::Utc::now()));
        active.updated_at = Set(chrono::Utc::now());
        active.update(txn).await?;
        Ok(true)
    }

    pub async fn delete_project(
        &self,
        project_id: i32,
        project_name: &str,
    ) -> Result<(), ProjectError> {
        self.ensure_no_delivery_bindings(project_id).await?;

        // Fetch environments before deletion to emit cleanup jobs.
        // We only need id, name, and project_id — use select_only to avoid loading full models.
        let environments_to_delete: Vec<(i32, String, i32)> =
            temps_entities::environments::Entity::find()
                .filter(temps_entities::environments::Column::ProjectId.eq(project_id))
                .select_only()
                .column(temps_entities::environments::Column::Id)
                .column(temps_entities::environments::Column::Name)
                .column(temps_entities::environments::Column::ProjectId)
                .into_tuple()
                .all(self.db.as_ref())
                .await
                .map_err(|e| ProjectError::Other(e.to_string()))?;

        // Emit EnvironmentDeleted jobs before deletion so subscribers can clean up
        for (env_id, env_name, env_project_id) in &environments_to_delete {
            let env_deleted_job = Job::EnvironmentDeleted(temps_core::EnvironmentDeletedJob {
                environment_id: *env_id,
                environment_name: env_name.clone(),
                project_id: *env_project_id,
            });

            if let Err(e) = self.queue_service.send(env_deleted_job).await {
                warn!(
                    "Failed to emit EnvironmentDeleted job for environment {}: {}",
                    env_id, e
                );
            }
        }

        // Delete the project row. Control-plane data (deployments, environments,
        // domains, crons, env_vars, services, etc.) goes with it through
        // ON DELETE CASCADE. The high-volume telemetry tables hold no foreign
        // key to the project, so they are not part of this statement: their
        // rows are purged in the background, from the purge row committed in
        // the same transaction, so the project can't disappear without one.
        let txn = self.db.begin().await?;
        let deleted = temps_entities::projects::Entity::delete_by_id(project_id)
            .exec(&txn)
            .await?;
        if deleted.rows_affected > 0 {
            super::data_purge::ProjectDataPurger::enqueue(&txn, project_id, project_name).await?;
        }
        txn.commit().await?;

        // Emit ProjectDeleted job for async cleanup (e.g. status monitors)
        let project_deleted_job = Job::ProjectDeleted(ProjectDeletedJob {
            project_id,
            project_name: project_name.to_string(),
        });

        if let Err(e) = self.queue_service.send(project_deleted_job).await {
            warn!(
                "Failed to emit ProjectDeleted job for project {}: {}",
                project_id, e
            );
        }

        info!(
            "Project {} and all related data deleted successfully",
            project_id
        );

        Ok(())
    }
}
