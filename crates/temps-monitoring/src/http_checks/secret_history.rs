// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! History and automatic detection for project secrets.
use super::*;
use sea_orm::FromQueryResult;
use temps_entities::secret_history;

/// Secrets may hold up to 1 MiB, but no check kind inspects more than local
/// inspection's 64 KiB (issuer tokens are at most 16 KiB), so larger values are
/// never decrypted by detection. Stored ciphertext is base64(nonce || data || tag).
const MAX_SCANNED_CIPHERTEXT_BYTES: i64 = (MAX_LOCAL_INPUT_BYTES as i64 + 64) * 4 / 3 + 4;

#[derive(FromQueryResult)]
struct SecretCandidate {
    id: i32,
    project_id: i32,
    key: String,
    /// NULL when the ciphertext exceeds `MAX_SCANNED_CIPHERTEXT_BYTES`.
    value: Option<String>,
}

impl HttpChecksService {
    pub async fn secret_history(
        &self,
        project_id: i32,
        secret_id: i32,
        page: u64,
        page_size: u64,
    ) -> Result<VariableHistoryList, HttpChecksError> {
        let exists = secrets::Entity::find_by_id(secret_id)
            .filter(secrets::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await
            .map_err(|e| db_error(project_id, "find secret history", e))?;
        if exists.is_none() {
            return Err(HttpChecksError::SecretNotFound {
                project_id,
                secret_id,
            });
        }
        let page = page.max(1);
        let page_size = page_size.clamp(1, 100);
        let query = secret_history::Entity::find()
            .filter(secret_history::Column::ProjectId.eq(project_id))
            .filter(secret_history::Column::SecretId.eq(secret_id))
            .order_by_desc(secret_history::Column::Id)
            .paginate(self.db.as_ref(), page_size);
        let total = query
            .num_items()
            .await
            .map_err(|e| db_error(project_id, "count secret history", e))?;
        let items = query
            .fetch_page(page - 1)
            .await
            .map_err(|e| db_error(project_id, "list secret history", e))?
            .into_iter()
            .map(|row| {
                let details = serde_json::from_value(row.details).map_err(|_| {
                    HttpChecksError::SecretHistoryStored {
                        project_id,
                        secret_id,
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

    /// Lock a bounded batch of unscanned secrets so replicas cannot duplicate
    /// automatic checks. A new value or key removes the marker (database trigger).
    /// `FOR NO KEY UPDATE` excludes other reconcilers and secret writes but not the
    /// key-share lock a running check takes when it records history.
    pub async fn reconcile_secrets(&self) -> Result<(), HttpChecksError> {
        let tx = self
            .db
            .begin()
            .await
            .map_err(|e| db_error(0, "begin automatic secret detection", e))?;
        let candidates = SecretCandidate::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT s.id,s.project_id,s.key,CASE WHEN octet_length(s.value) <= $1 THEN s.value END AS value FROM secrets s LEFT JOIN secret_check_detection d ON d.secret_id=s.id WHERE d.secret_id IS NULL OR d.retry_after <= NOW() ORDER BY COALESCE(d.retry_after,'-infinity'::timestamptz), s.id LIMIT 20 FOR NO KEY UPDATE OF s SKIP LOCKED",
            [MAX_SCANNED_CIPHERTEXT_BYTES.into()],
        ))
        .all(&tx)
        .await
        .map_err(|e| db_error(0, "find unscanned secrets", e))?;
        for secret in candidates {
            let project_id = secret.project_id;
            let decrypted = secret
                .value
                .as_deref()
                .map(|ciphertext| self.encryption.decrypt_string(ciphertext));
            match decrypted {
                Some(Ok(value)) => {
                    self.apply_automatic_check(
                        &tx,
                        &super::automatic::SECRET,
                        project_id,
                        secret.id,
                        &secret.key,
                        &value,
                    )
                    .await?;
                }
                Some(Err(_)) => {
                    // A corrupt secret must not block detection for the others. History
                    // records the first failure only, not every five-minute retry.
                    tx.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,"INSERT INTO secret_history(project_id,secret_id,kind) SELECT $1,$2,'detection_unavailable' WHERE NOT EXISTS(SELECT 1 FROM secret_check_detection WHERE secret_id=$2 AND retry_after IS NOT NULL)",[project_id.into(),secret.id.into()])).await.map_err(|e|db_error(project_id,"record unavailable secret detection",e))?;
                    tx.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,"INSERT INTO secret_check_detection(secret_id,retry_after) VALUES($1,NOW()+INTERVAL '5 minutes') ON CONFLICT(secret_id) DO UPDATE SET retry_after=EXCLUDED.retry_after",[secret.id.into()])).await.map_err(|e|db_error(project_id,"record unavailable secret detection marker",e))?;
                    tracing::warn!(
                        project_id,
                        secret_id = secret.id,
                        "Automatic credential detection could not decrypt secret"
                    );
                    continue;
                }
                None => {
                    // Too large to hold an inspectable credential: drop a stale automatic check.
                    tx.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,"DELETE FROM http_checks WHERE secret_id=$1 AND automatic_provider IS NOT NULL",[secret.id.into()])).await.map_err(|e|db_error(project_id,"remove automatic check for oversized secret",e))?;
                }
            }
            tx.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,"INSERT INTO secret_check_detection(secret_id) VALUES($1) ON CONFLICT(secret_id) DO UPDATE SET retry_after=NULL",[secret.id.into()])).await.map_err(|e|db_error(project_id,"record automatic secret detection",e))?;
        }
        tx.commit()
            .await
            .map_err(|e| db_error(0, "commit automatic secret detection", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scan_bound_admits_every_inspectable_certificate() {
        let encryption = EncryptionService::new_from_password("test-only-not-a-live-credential");
        let largest = encryption
            .encrypt_string(&"A".repeat(MAX_LOCAL_INPUT_BYTES))
            .unwrap();
        assert!(largest.len() as i64 <= MAX_SCANNED_CIPHERTEXT_BYTES);
        let oversized = encryption
            .encrypt_string(&"A".repeat(MAX_LOCAL_INPUT_BYTES + 128))
            .unwrap();
        assert!(oversized.len() as i64 > MAX_SCANNED_CIPHERTEXT_BYTES);
    }
}
