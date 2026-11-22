// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! No plaintext key cache: every use checks the durable key/tombstone. In-flight
//! users may finish after shredding; later operations cannot recover the key.
use crate::error::{WorkflowError, WorkflowResult};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait, FromQueryResult,
    PaginatorTrait, QueryFilter, QueryOrder, Statement,
};
use std::sync::Arc;
use temps_core::EncryptionService;
use temps_entities::workflow_project_keys::{self, Model};
use uuid::Uuid;

pub struct ProjectKeys {
    db: Arc<DatabaseConnection>,
    encryption: Arc<EncryptionService>,
}
impl ProjectKeys {
    pub fn new(db: Arc<DatabaseConnection>, encryption: Arc<EncryptionService>) -> Self {
        Self { db, encryption }
    }
    pub async fn active(&self, project_id: i32) -> WorkflowResult<Model> {
        if let Some(key) = workflow_project_keys::Entity::find()
            .filter(workflow_project_keys::Column::ProjectId.eq(project_id))
            .filter(workflow_project_keys::Column::ShreddedAt.is_null())
            .one(self.db.as_ref())
            .await
            .map_err(WorkflowError::db("reading active project encryption key"))?
        {
            return Ok(key);
        }
        let id = Uuid::new_v4();
        let wrapped = self
            .encryption
            .create_project_data_key(project_id, &id.to_string())
            .map_err(|source| WorkflowError::Encryption {
                run_id: Uuid::nil(),
                field: format!("project {project_id} data key"),
                operation: "wrap",
                source,
            })?;
        // Atomic uniqueness resolves concurrent first writes without replacing a key.
        let row = Model::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "INSERT INTO workflow_project_keys (id, project_id, wrapped_key) VALUES ($1,$2,$3)
             ON CONFLICT (project_id) WHERE shredded_at IS NULL DO UPDATE SET project_id=EXCLUDED.project_id RETURNING *",
            vec![id.into(),project_id.into(),wrapped.into()])).one(self.db.as_ref()).await.map_err(WorkflowError::db("creating project encryption key"))?;
        row.ok_or(WorkflowError::ProjectKeyUnavailable {
            project_id,
            key_id: id,
        })
    }
    pub async fn get(&self, project_id: i32, key_id: Uuid) -> WorkflowResult<Model> {
        workflow_project_keys::Entity::find_by_id(key_id)
            .filter(workflow_project_keys::Column::ProjectId.eq(project_id))
            .filter(workflow_project_keys::Column::ShreddedAt.is_null())
            .one(self.db.as_ref())
            .await
            .map_err(WorkflowError::db("reading project encryption key"))?
            .ok_or(WorkflowError::ProjectKeyUnavailable { project_id, key_id })
    }
    pub async fn list(
        &self,
        project_id: i32,
        page: u64,
        page_size: u64,
    ) -> WorkflowResult<(Vec<Model>, u64)> {
        let paginator = workflow_project_keys::Entity::find()
            .filter(workflow_project_keys::Column::ProjectId.eq(project_id))
            .order_by_desc(workflow_project_keys::Column::CreatedAt)
            .order_by_desc(workflow_project_keys::Column::Id)
            .paginate(self.db.as_ref(), page_size.clamp(1, 100));
        let total = paginator
            .num_items()
            .await
            .map_err(WorkflowError::db("counting project encryption keys"))?;
        let keys = paginator
            .fetch_page(page.saturating_sub(1))
            .await
            .map_err(WorkflowError::db("listing project encryption keys"))?;
        Ok((keys, total))
    }
    pub async fn shred(&self, project_id: i32, key_id: Uuid) -> WorkflowResult<()> {
        let result = self.db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
            "UPDATE workflow_project_keys SET wrapped_key=NULL, shredded_at=COALESCE(shredded_at,now()) WHERE project_id=$1 AND id=$2", vec![project_id.into(),key_id.into()])).await.map_err(WorkflowError::db("shredding project encryption key"))?;
        if result.rows_affected() == 0 {
            return Err(WorkflowError::ProjectKeyUnavailable { project_id, key_id });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::{PayloadCodec, PayloadField, PayloadScope};
    use crate::types::WorkflowSettings;
    use serde_json::json;

    #[tokio::test]
    async fn project_keys_survive_restart_but_not_shredding_and_legacy_payloads_still_open() {
        let Some(env) = crate::test_support::test_environment("project keys").await else {
            return;
        };
        let encryption = crate::payload::tests::encryption();
        let keys = Arc::new(ProjectKeys::new(env.conn.clone(), encryption.clone()));
        let codec = PayloadCodec::new(encryption.clone(), None).with_project_keys(keys.clone());
        let legacy = PayloadCodec::new(encryption.clone(), None);
        let settings = WorkflowSettings::default();
        let scope = PayloadScope {
            project_id: env.project_id,
            environment_id: env.environment_id,
            run_id: Uuid::new_v4(),
        };
        let value = json!({"sample":"private"});
        let old = legacy
            .seal(&scope, &PayloadField::RunInput, &value, 1, &settings)
            .await
            .unwrap();
        let (a, b) = tokio::join!(keys.active(env.project_id), keys.active(env.project_id));
        let id = a.unwrap().id;
        assert_eq!(id, b.unwrap().id);
        let stored = codec
            .seal(&scope, &PayloadField::RunInput, &value, 1, &settings)
            .await
            .unwrap();
        assert!(stored.encrypted.as_ref().unwrap().starts_with("pk1:"));
        let restarted = PayloadCodec::new(encryption.clone(), None)
            .with_project_keys(Arc::new(ProjectKeys::new(env.conn.clone(), encryption)));
        assert_eq!(
            restarted
                .open(&scope, &PayloadField::RunInput, &stored, &settings)
                .await
                .unwrap(),
            Some(value.clone())
        );
        assert!(keys.get(env.project_id + 1, id).await.is_err());
        keys.shred(env.project_id, id).await.unwrap();
        assert!(matches!(
            restarted
                .open(&scope, &PayloadField::RunInput, &stored, &settings)
                .await,
            Err(WorkflowError::ProjectKeyUnavailable { .. })
        ));
        assert_eq!(
            restarted
                .open(&scope, &PayloadField::RunInput, &old, &settings)
                .await
                .unwrap(),
            Some(value.clone())
        );
        let next = restarted
            .seal(&scope, &PayloadField::RunOutput, &value, 2, &settings)
            .await
            .unwrap();
        assert_eq!(
            restarted
                .open(&scope, &PayloadField::RunOutput, &next, &settings)
                .await
                .unwrap(),
            Some(value)
        );
        assert_ne!(keys.active(env.project_id).await.unwrap().id, id);
        let (history, total) = keys.list(env.project_id, 1, 20).await.unwrap();
        assert_eq!(total, 2);
        assert!(history
            .iter()
            .find(|k| k.id == id)
            .unwrap()
            .wrapped_key
            .is_none());
        // Downgrade must never discard the only keys capable of reading data.
        use sea_orm_migration::MigratorTrait;
        use temps_migrations::Migrator;
        let migration = Migrator::migrations()
            .into_iter()
            .find(|m| m.name() == "m20261113_000003_workflow_project_keys")
            .unwrap();
        let manager = sea_orm_migration::SchemaManager::new(env.conn.as_ref());
        assert!(migration.down(&manager).await.is_err());
        env.conn
            .execute_unprepared("DELETE FROM workflow_project_keys")
            .await
            .unwrap();
        migration.down(&manager).await.unwrap();
        migration.up(&manager).await.unwrap();
        assert_eq!(keys.list(env.project_id, 1, 20).await.unwrap().1, 0);
    }
}
