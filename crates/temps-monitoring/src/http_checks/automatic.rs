// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Automatic-check maintenance shared by environment variables and secrets.
use super::*;
use sea_orm::DatabaseTransaction;
use temps_credential_checks::AutomaticCheck;

/// SQL identifiers that differ between credential subjects. These are
/// interpolated into statements, so they must only ever be static literals.
pub(super) struct Subject {
    pub column: &'static str,
    pub suppressions: &'static str,
}
pub(super) const VARIABLE: Subject = Subject {
    column: "env_var_id",
    suppressions: "env_check_suppressions",
};
pub(super) const SECRET: Subject = Subject {
    column: "secret_id",
    suppressions: "secret_check_suppressions",
};

impl HttpChecksService {
    /// Creates, replaces or removes the automatic check for one credential.
    /// Manual checks take precedence, explicit deletions (suppressions) are
    /// honoured, and a paused automatic check stays paused.
    pub(super) async fn apply_automatic_check(
        &self,
        tx: &DatabaseTransaction,
        subject: &Subject,
        project_id: i32,
        id: i32,
        key: &str,
        value: &str,
    ) -> Result<(), HttpChecksError> {
        let Subject {
            column,
            suppressions,
        } = subject;
        tx.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            format!("DELETE FROM http_checks WHERE {column}=$1 AND automatic_provider IS NOT NULL AND EXISTS(SELECT 1 FROM http_checks manual WHERE manual.{column}=$1 AND manual.automatic_provider IS NULL)"),
            [id.into()],
        ))
        .await
        .map_err(|e| db_error(project_id, "replace automatic check with custom check", e))?;
        let candidates = self.detector.detect(key, value);
        let Some(check) = temps_credential_checks::automatic_check(&candidates, value) else {
            // A renamed/replaced credential must never keep being sent to its former issuer.
            tx.execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!(
                    "DELETE FROM http_checks WHERE {column}=$1 AND automatic_provider IS NOT NULL"
                ),
                [id.into()],
            ))
            .await
            .map_err(|e| db_error(project_id, "remove obsolete automatic check", e))?;
            return Ok(());
        };
        let spec = match &check {
            AutomaticCheck::Http(preset) => serde_json::to_string(&preset.spec),
            AutomaticCheck::Local(spec) => serde_json::to_string(spec),
        }
        .map_err(|_| HttpChecksError::Stored { id })?;
        let encrypted = self
            .encryption
            .encrypt_string(&spec)
            .map_err(|_| HttpChecksError::Encryption { project_id })?;
        tx.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            format!("INSERT INTO http_checks(project_id,{column},kind,name,encrypted_spec,automatic_provider) SELECT $1,$2,$6,$3,$4,$5 WHERE NOT EXISTS(SELECT 1 FROM http_checks WHERE {column}=$2 AND automatic_provider IS NULL) AND NOT EXISTS(SELECT 1 FROM {suppressions} WHERE {column}=$2 AND automatic_provider IN ($5,'*')) ON CONFLICT({column}) WHERE automatic_provider IS NOT NULL DO UPDATE SET automatic_provider=EXCLUDED.automatic_provider,kind=EXCLUDED.kind,encrypted_spec=EXCLUDED.encrypted_spec,name=EXCLUDED.name,last_result=NULL,last_checked_at=NULL,lease_token=NULL,lease_until=NULL,next_check_at=NOW() WHERE http_checks.automatic_provider IS DISTINCT FROM EXCLUDED.automatic_provider"),
            [
                project_id.into(),
                id.into(),
                check.check_name().into(),
                encrypted.into(),
                check.provider().to_owned().into(),
                check.kind().as_str().into(),
            ],
        ))
        .await
        .map_err(|e| db_error(project_id, "create automatic check", e))?;
        Ok(())
    }
}
