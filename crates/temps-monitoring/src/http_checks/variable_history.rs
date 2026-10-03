// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use temps_entities::env_var_history;

#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct VariableHistoryDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<VerificationResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_secret: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_in_preview: Option<bool>,
    /// Secret scope after a `scope_changed` event; empty means every environment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment_ids: Option<Vec<i32>>,
    /// Compose services a secret is limited to after a `scope_changed` event.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compose_services: Option<Vec<String>>,
}

#[derive(Serialize, ToSchema)]
pub struct VariableHistoryEntry {
    pub id: i64,
    pub kind: String,
    pub details: VariableHistoryDetails,
    #[schema(value_type=String,format=DateTime)]
    pub created_at: DateTime<Utc>,
}
#[derive(Serialize, ToSchema)]
pub struct VariableHistoryList {
    pub items: Vec<VariableHistoryEntry>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}
impl HttpChecksService {
    pub async fn variable_history(
        &self,
        project_id: i32,
        env_var_id: i32,
        page: u64,
        page_size: u64,
    ) -> Result<VariableHistoryList, HttpChecksError> {
        let exists = env_vars::Entity::find_by_id(env_var_id)
            .filter(env_vars::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await
            .map_err(|e| db_error(project_id, "find variable history", e))?;
        if exists.is_none() {
            return Err(HttpChecksError::NotFound {
                project_id,
                id: env_var_id,
            });
        }
        let page = page.max(1);
        let page_size = page_size.clamp(1, 100);
        let query = env_var_history::Entity::find()
            .filter(env_var_history::Column::ProjectId.eq(project_id))
            .filter(env_var_history::Column::EnvVarId.eq(env_var_id))
            .order_by_desc(env_var_history::Column::Id)
            .paginate(self.db.as_ref(), page_size);
        let total = query
            .num_items()
            .await
            .map_err(|e| db_error(project_id, "count variable history", e))?;
        let items = query
            .fetch_page(page - 1)
            .await
            .map_err(|e| db_error(project_id, "list variable history", e))?
            .into_iter()
            .map(|row| {
                let details = serde_json::from_value(row.details).map_err(|_| {
                    HttpChecksError::HistoryStored {
                        project_id,
                        env_var_id,
                        entry_id: row.id,
                    }
                })?;
                Ok(VariableHistoryEntry {
                    id: row.id,
                    kind: row.kind,
                    details,
                    created_at: row.created_at,
                })
            })
            .collect::<Result<Vec<_>, HttpChecksError>>()?;
        Ok(VariableHistoryList {
            items,
            total,
            page,
            page_size,
        })
    }
    /// Lock a bounded batch of variables so replicas cannot duplicate automatic checks.
    /// Each scan marker and its check are committed together; rotation removes the marker.
    pub async fn reconcile_variables(&self) -> Result<(), HttpChecksError> {
        let tx = self
            .db
            .begin()
            .await
            .map_err(|e| db_error(0, "begin automatic detection", e))?;
        let variables=env_vars::Entity::find().from_raw_sql(Statement::from_string(DatabaseBackend::Postgres,
            "SELECT e.* FROM env_vars e LEFT JOIN env_check_detection d ON d.env_var_id=e.id WHERE d.env_var_id IS NULL OR d.retry_after <= NOW() ORDER BY COALESCE(d.retry_after, '-infinity'::timestamptz), e.id LIMIT 20 FOR NO KEY UPDATE OF e SKIP LOCKED")).all(&tx).await.map_err(|e|db_error(0,"find unscanned variables",e))?;
        for variable in variables {
            let project_id = variable.project_id;
            let value = if variable.is_encrypted {
                match self.encryption.decrypt_string(&variable.value) {
                    Ok(value) => value,
                    Err(_) => {
                        // A corrupt credential must not block detection for other variables.
                        // History records the first failure only, not every retry.
                        tx.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,"INSERT INTO env_var_history(project_id,env_var_id,kind) SELECT $1,$2,'detection_unavailable' WHERE NOT EXISTS(SELECT 1 FROM env_check_detection WHERE env_var_id=$2 AND retry_after IS NOT NULL)",[project_id.into(),variable.id.into()])).await.map_err(|e|db_error(project_id,"record unavailable detection",e))?;
                        tx.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,"INSERT INTO env_check_detection(env_var_id,observed_updated_at,retry_after) VALUES($1,$2,NOW()+INTERVAL '5 minutes') ON CONFLICT(env_var_id) DO UPDATE SET retry_after=EXCLUDED.retry_after",[variable.id.into(),variable.updated_at.into()])).await.map_err(|e|db_error(project_id,"record unavailable detection marker",e))?;
                        tracing::warn!(
                            project_id,
                            env_var_id = variable.id,
                            "Automatic credential detection could not decrypt variable"
                        );
                        continue;
                    }
                }
            } else {
                variable.value
            };
            self.apply_automatic_check(
                &tx,
                &super::automatic::VARIABLE,
                project_id,
                variable.id,
                &variable.key,
                &value,
            )
            .await?;
            tx.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,"INSERT INTO env_check_detection(env_var_id,observed_updated_at) VALUES($1,$2) ON CONFLICT(env_var_id) DO UPDATE SET observed_updated_at=EXCLUDED.observed_updated_at,retry_after=NULL",[variable.id.into(),variable.updated_at.into()])).await.map_err(|e|db_error(project_id,"record automatic detection",e))?;
        }
        tx.commit()
            .await
            .map_err(|e| db_error(0, "commit automatic detection", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_details_only_serialize_known_fields() {
        let details: VariableHistoryDetails = serde_json::from_value(
            serde_json::json!({"check_name":"GitHub","unexpected_secret":"must-not-leave-storage"}),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(details).unwrap(),
            serde_json::json!({"check_name":"GitHub"})
        );
        assert!(serde_json::from_value::<VariableHistoryDetails>(
            serde_json::json!({"result":"invalid"})
        )
        .is_err());
        assert_eq!(
            serde_json::to_value(VariableHistoryDetails::default()).unwrap(),
            serde_json::json!({})
        );
    }
}
