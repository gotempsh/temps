// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Saved Cloudflare Access tokens (ADR 065 D9): checked before they are
//! saved, sealed with `EncryptionService`, never returned, and unsealed
//! only for the plan and the run that use them.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sea_orm::{
    sea_query::Expr, ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, Set,
};
use serde::{Deserialize, Serialize};
use temps_core::EncryptionService;
use temps_entities::{
    cloudflare_access_providers, environment_domains, project_custom_domains, reachability_runs,
};
use temps_routes::access_keys::AccessKeyService;
use thiserror::Error;
use tracing::warn;
use utoipa::ToSchema;

use super::access::{
    self, check_access_token, AccessApp, AccessPolicyAnswer, AccessTokenCheckResult, CreateAppError,
};
use super::model::{Intent, JournalEntry, JournalState};

#[derive(Debug, Error)]
pub enum AccessProviderError {
    #[error("Cloudflare Access token {provider_id} not found")]
    NotFound { provider_id: i32 },
    #[error("Validation error: {message}")]
    Validation { message: String },
    #[error("The Cloudflare Access token was not saved: {reason}")]
    TokenRefused {
        reason: String,
        check: Box<AccessTokenCheckResult>,
    },
    #[error(
        "Cloudflare Access token {provider_id} protects {addresses} address(es); switch them to \
         the Temps sign-in or remove them before deleting it"
    )]
    InUse { provider_id: i32, addresses: u64 },
    #[error(
        "Cloudflare Access token {provider_id} is still needed to undo run(s) {}, which created \
         an Access app with it; undo them first (or delete the app in Cloudflare, then undo \
         them) before deleting the token",
        run_list(.runs)
    )]
    NeededToUndo { provider_id: i32, runs: Vec<i32> },
    #[error("Failed to {operation} the Cloudflare Access token {provider_id:?}: {reason}")]
    Encryption {
        operation: &'static str,
        provider_id: Option<i32>,
        reason: String,
    },
    #[error("Failed to {operation} for Cloudflare Access tokens: {source}")]
    Database {
        operation: &'static str,
        #[source]
        source: sea_orm::DbErr,
    },
}

fn run_list(runs: &[i32]) -> String {
    runs.iter()
        .map(i32::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether undo may still have to delete an Access app of `provider_id`
/// that this entry recorded: everything but an app found never made or
/// already gone, and a conflict found before anything was written.
fn holds_access_app(entry: &JournalEntry, provider_id: i32) -> bool {
    let Intent::AccessApp {
        provider_id: id, ..
    } = &entry.intent
    else {
        return false;
    };
    *id == provider_id
        && match entry.state {
            JournalState::Absent => false,
            JournalState::Conflict => !entry.unwritten,
            JournalState::Pending | JournalState::Done => true,
        }
}

/// Whether a run's journal still holds an Access app of `provider_id`. A
/// journal that does not decode counts as holding one: refusing a delete
/// is recoverable, stranding an app is not.
fn journal_holds_access_app(journal: &serde_json::Value, provider_id: i32) -> bool {
    serde_json::from_value::<Vec<JournalEntry>>(journal.clone())
        .map(|entries| entries.iter().any(|e| holds_access_app(e, provider_id)))
        .unwrap_or(true)
}

fn db(operation: &'static str) -> impl FnOnce(sea_orm::DbErr) -> AccessProviderError {
    move |source| AccessProviderError::Database { operation, source }
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateAccessProviderRequest {
    /// A name to tell tokens apart.
    pub name: String,
    pub account_id: String,
    /// Checked, then sealed; never returned.
    pub token: temps_core::secret_string::SecretString,
}

/// A saved Access token. The token itself is never returned.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AccessProviderResponse {
    pub id: i32,
    pub name: String,
    pub account_id: String,
    /// `<team>.cloudflareaccess.com`.
    pub team_domain: String,
    /// Always `***`.
    pub api_token: String,
    /// When the team's signing keys were last fetched.
    #[schema(value_type = Option<String>, format = DateTime)]
    pub keys_fetched_at: Option<DateTime<Utc>>,
    /// How many signing keys listeners have for it now.
    pub key_count: usize,
    /// How many addresses it protects.
    pub addresses: u64,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: DateTime<Utc>,
}

/// What a plan or run needs from a saved token.
#[derive(Clone)]
pub struct AccessCredentials {
    pub provider_id: i32,
    pub account_id: String,
    pub team_domain: String,
    pub token: String,
}

impl std::fmt::Debug for AccessCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccessCredentials")
            .field("provider_id", &self.provider_id)
            .field("account_id", &self.account_id)
            .field("team_domain", &self.team_domain)
            .field("token", &"***")
            .finish()
    }
}

pub struct AccessProviderService {
    db: Arc<DatabaseConnection>,
    encryption: Arc<EncryptionService>,
    keys: Arc<AccessKeyService>,
    http: reqwest::Client,
}

impl AccessProviderService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        encryption: Arc<EncryptionService>,
        keys: Arc<AccessKeyService>,
        http: reqwest::Client,
    ) -> Self {
        Self {
            db,
            encryption,
            keys,
            http,
        }
    }

    /// Check a token without saving it.
    pub async fn check(&self, token: &str, account_id: &str) -> AccessTokenCheckResult {
        check_access_token(&self.http, token, account_id).await
    }

    async fn addresses_using(&self, provider_id: i32) -> Result<u64, AccessProviderError> {
        let custom = project_custom_domains::Entity::find()
            .filter(project_custom_domains::Column::GateAccessProviderId.eq(provider_id))
            .count(self.db.as_ref())
            .await
            .map_err(db("count the custom domains using a token"))?;
        let environment = environment_domains::Entity::find()
            .filter(environment_domains::Column::GateAccessProviderId.eq(provider_id))
            .count(self.db.as_ref())
            .await
            .map_err(db("count the environment domains using a token"))?;
        Ok(custom + environment)
    }

    /// The runs whose undo still needs the token to delete an Access app
    /// they created. Only journals mentioning an app of this token are
    /// read (a `jsonb` containment on the intent), so this is bounded by
    /// the runs that used it, not by every run.
    async fn runs_needing(&self, provider_id: i32) -> Result<Vec<i32>, AccessProviderError> {
        let needle = serde_json::json!([
            { "intent": { "type": "access_app", "provider_id": provider_id } }
        ]);
        let rows = reachability_runs::Entity::find()
            .filter(Expr::cust_with_values(
                "journal @> $1::jsonb",
                [needle.to_string()],
            ))
            .order_by_asc(reachability_runs::Column::Id)
            .all(self.db.as_ref())
            .await
            .map_err(db("find the runs that created an Access app with a token"))?;
        Ok(rows
            .into_iter()
            .filter(|row| journal_holds_access_app(&row.journal, provider_id))
            .map(|row| row.id)
            .collect())
    }

    async fn respond(
        &self,
        model: cloudflare_access_providers::Model,
    ) -> Result<AccessProviderResponse, AccessProviderError> {
        Ok(AccessProviderResponse {
            addresses: self.addresses_using(model.id).await?,
            key_count: temps_routes::access_keys::stored_keys(model.jwks.as_ref()).len(),
            id: model.id,
            name: model.name,
            account_id: model.account_id,
            team_domain: model.team_domain,
            api_token: "***".to_string(),
            keys_fetched_at: model.jwks_fetched_at,
            created_at: model.created_at,
        })
    }

    /// Every saved token, newest first. There are a handful at most, so
    /// this is not paginated.
    pub async fn list(&self) -> Result<Vec<AccessProviderResponse>, AccessProviderError> {
        let models = cloudflare_access_providers::Entity::find()
            .order_by_desc(cloudflare_access_providers::Column::CreatedAt)
            .all(self.db.as_ref())
            .await
            .map_err(db("list the tokens"))?;
        let mut out = Vec::with_capacity(models.len());
        for model in models {
            out.push(self.respond(model).await?);
        }
        Ok(out)
    }

    /// The saved tokens as `(id, name, team domain)`, for plans.
    pub async fn names(&self) -> Result<Vec<(i32, String, String)>, AccessProviderError> {
        Ok(cloudflare_access_providers::Entity::find()
            .order_by_asc(cloudflare_access_providers::Column::Id)
            .all(self.db.as_ref())
            .await
            .map_err(db("list the tokens"))?
            .into_iter()
            .map(|m| (m.id, m.name, m.team_domain))
            .collect())
    }

    /// Check the token, then save it sealed, then fetch the team's signing
    /// keys (a failure there is logged; the hourly refresh retries).
    pub async fn create(
        &self,
        request: &CreateAccessProviderRequest,
        user_id: Option<i32>,
    ) -> Result<AccessProviderResponse, AccessProviderError> {
        let name = request.name.trim();
        if name.is_empty() || name.len() > 255 {
            return Err(AccessProviderError::Validation {
                message: "Give the token a name of 1 to 255 characters.".into(),
            });
        }
        let account_id = request.account_id.trim().to_ascii_lowercase();
        let check = self.check(request.token.expose(), &account_id).await;
        if let Some(reason) = check.refusal() {
            return Err(AccessProviderError::TokenRefused {
                reason,
                check: Box::new(check),
            });
        }
        let Some(team_domain) = check.team_domain.clone() else {
            return Err(AccessProviderError::Validation {
                message: "Cloudflare did not say which Access team the account uses.".into(),
            });
        };
        let sealed = self
            .encryption
            .encrypt_string(request.token.expose().trim())
            .map_err(|e| AccessProviderError::Encryption {
                operation: "seal",
                provider_id: None,
                reason: e.to_string(),
            })?;
        let now = Utc::now();
        let model = cloudflare_access_providers::ActiveModel {
            name: Set(name.to_string()),
            account_id: Set(account_id),
            team_domain: Set(team_domain),
            api_token_encrypted: Set(sealed),
            jwks: Set(None),
            jwks_fetched_at: Set(None),
            created_by: Set(user_id),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await
        .map_err(db("save the token"))?;
        let model = match self.keys.refresh(model.id, true).await {
            Ok(_) => cloudflare_access_providers::Entity::find_by_id(model.id)
                .one(self.db.as_ref())
                .await
                .map_err(db("read the saved token"))?
                .unwrap_or(model),
            Err(error) => {
                warn!(provider_id = model.id, %error, "could not fetch the Access signing keys of a new token");
                model
            }
        };
        self.respond(model).await
    }

    /// Delete a token no address uses and no run still needs to undo an
    /// Access app it created: once the token is gone, that undo could
    /// never reach the app again.
    pub async fn delete(
        &self,
        provider_id: i32,
    ) -> Result<cloudflare_access_providers::Model, AccessProviderError> {
        let model = cloudflare_access_providers::Entity::find_by_id(provider_id)
            .one(self.db.as_ref())
            .await
            .map_err(db("load the token"))?
            .ok_or(AccessProviderError::NotFound { provider_id })?;
        let addresses = self.addresses_using(provider_id).await?;
        if addresses > 0 {
            return Err(AccessProviderError::InUse {
                provider_id,
                addresses,
            });
        }
        let runs = self.runs_needing(provider_id).await?;
        if !runs.is_empty() {
            return Err(AccessProviderError::NeededToUndo { provider_id, runs });
        }
        cloudflare_access_providers::Entity::delete_by_id(provider_id)
            .exec(self.db.as_ref())
            .await
            .map_err(|source| match source.sql_err() {
                // An address was bound to it since the count above.
                Some(sea_orm::SqlErr::ForeignKeyConstraintViolation(_)) => {
                    AccessProviderError::InUse {
                        provider_id,
                        addresses: 1,
                    }
                }
                _ => AccessProviderError::Database {
                    operation: "delete the token",
                    source,
                },
            })?;
        Ok(model)
    }

    /// The token, unsealed, for a plan or a run.
    pub async fn credentials(
        &self,
        provider_id: i32,
    ) -> Result<AccessCredentials, AccessProviderError> {
        let model = cloudflare_access_providers::Entity::find_by_id(provider_id)
            .one(self.db.as_ref())
            .await
            .map_err(db("load the token"))?
            .ok_or(AccessProviderError::NotFound { provider_id })?;
        let token = self
            .encryption
            .decrypt_string(&model.api_token_encrypted)
            .map_err(|e| AccessProviderError::Encryption {
                operation: "unseal",
                provider_id: Some(provider_id),
                reason: e.to_string(),
            })?;
        Ok(AccessCredentials {
            provider_id,
            account_id: model.account_id,
            team_domain: model.team_domain,
            token,
        })
    }

    /// The apps in `provider_id`'s account that protect every path of
    /// `host`, for a plan (ADR 065 D9).
    pub async fn covering_apps(
        &self,
        provider_id: i32,
        host: &str,
    ) -> Result<Vec<AccessApp>, String> {
        let credentials = self
            .credentials(provider_id)
            .await
            .map_err(|e| e.to_string())?;
        let apps =
            access::list_apps(&self.http, &credentials.token, &credentials.account_id).await?;
        Ok(access::covering(apps, host))
    }

    /// The app named `name` protecting `host`: how a run finds the app it
    /// set out to create.
    pub async fn app_named(
        &self,
        provider_id: i32,
        name: &str,
        host: &str,
    ) -> Result<Option<AccessApp>, String> {
        let credentials = self
            .credentials(provider_id)
            .await
            .map_err(|e| e.to_string())?;
        let apps =
            access::list_apps(&self.http, &credentials.token, &credentials.account_id).await?;
        Ok(apps
            .into_iter()
            .find(|app| app.name == name && access::covers(app, host)))
    }

    pub async fn app(&self, provider_id: i32, id: &str) -> Result<Option<AccessApp>, String> {
        let credentials = self
            .credentials(provider_id)
            .await
            .map_err(|e| e.to_string())?;
        access::get_app(&self.http, &credentials.token, &credentials.account_id, id).await
    }

    pub async fn create_app(
        &self,
        provider_id: i32,
        name: &str,
        host: &str,
        policy: &AccessPolicyAnswer,
    ) -> Result<AccessApp, CreateAppError> {
        let credentials = self
            .credentials(provider_id)
            .await
            .map_err(|e| CreateAppError {
                message: e.to_string(),
                maybe_applied: false,
            })?;
        access::create_app(
            &self.http,
            &credentials.token,
            &credentials.account_id,
            access::app_body(name, host, policy),
        )
        .await
    }

    pub async fn delete_app(&self, provider_id: i32, id: &str) -> Result<(), String> {
        let credentials = self
            .credentials(provider_id)
            .await
            .map_err(|e| e.to_string())?;
        access::delete_app(&self.http, &credentials.token, &credentials.account_id, id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(db: DatabaseConnection) -> AccessProviderService {
        let db = Arc::new(db);
        AccessProviderService::new(
            db.clone(),
            Arc::new(EncryptionService::new_from_password("access-test")),
            Arc::new(AccessKeyService::new(db)),
            reqwest::Client::new(),
        )
    }

    #[test]
    fn credentials_never_print_the_token() {
        let credentials = AccessCredentials {
            provider_id: 1,
            account_id: "a".into(),
            team_domain: "team.cloudflareaccess.com".into(),
            token: "secret-token-value".into(),
        };
        assert!(!format!("{credentials:?}").contains("secret-token-value"));
    }

    #[tokio::test]
    async fn a_nameless_token_is_refused_before_cloudflare_is_called() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection();
        let result = service(db)
            .create(
                &CreateAccessProviderRequest {
                    name: "  ".into(),
                    account_id: "01a7362d577a6c3019a474fd6f485823".into(),
                    token: "t".into(),
                },
                Some(1),
            )
            .await;
        assert!(matches!(
            result,
            Err(AccessProviderError::Validation { .. })
        ));
    }

    fn token_row(id: i32) -> cloudflare_access_providers::Model {
        let now = Utc::now();
        cloudflare_access_providers::Model {
            id,
            name: "access".into(),
            account_id: "a".into(),
            team_domain: "team.cloudflareaccess.com".into(),
            api_token_encrypted: "sealed".into(),
            jwks: None,
            jwks_fetched_at: None,
            created_by: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn count(n: i64) -> Vec<std::collections::BTreeMap<String, sea_orm::Value>> {
        vec![std::collections::BTreeMap::from([(
            "num_items".to_string(),
            sea_orm::Value::BigInt(Some(n)),
        )])]
    }

    fn app_entry(provider_id: i32, state: JournalState) -> JournalEntry {
        let intent = Intent::AccessApp {
            provider_id,
            name: "temps-app.example.com-abc".into(),
            hostname: "app.example.com".into(),
        };
        JournalEntry {
            state,
            ..JournalEntry::pending(intent.key(0), intent, Utc::now())
        }
    }

    fn run_row(id: i32, journal: &[JournalEntry]) -> reachability_runs::Model {
        let now = Utc::now();
        reachability_runs::Model {
            id,
            project_id: Some(1),
            environment_id: Some(1),
            kind: "apply".into(),
            audience: "signed_in".into(),
            address: Some("app.example.com".into()),
            option: "tunnel".into(),
            plan_id: None,
            state: "succeeded".into(),
            steps: serde_json::json!([]),
            journal: serde_json::to_value(journal).expect("journal"),
            context: serde_json::json!({}),
            undo: None,
            error: None,
            live_url: None,
            created_by: None,
            created_at: now,
            updated_at: now,
            finished_at: Some(now),
        }
    }

    #[test]
    fn the_containment_needle_matches_how_an_access_app_is_journaled() {
        let entry = serde_json::to_value(app_entry(4, JournalState::Done)).expect("entry");
        assert_eq!(entry["intent"]["type"], "access_app");
        assert_eq!(entry["intent"]["provider_id"], 4);
    }

    #[test]
    fn only_an_app_undo_may_still_delete_holds_the_token() {
        assert!(holds_access_app(&app_entry(4, JournalState::Done), 4));
        assert!(holds_access_app(&app_entry(4, JournalState::Pending), 4));
        assert!(holds_access_app(&app_entry(4, JournalState::Conflict), 4));
        assert!(!holds_access_app(&app_entry(4, JournalState::Absent), 4));
        assert!(!holds_access_app(&app_entry(5, JournalState::Done), 4));
        let unwritten = JournalEntry {
            unwritten: true,
            ..app_entry(4, JournalState::Conflict)
        };
        assert!(!holds_access_app(&unwritten, 4));
        // A journal that does not decode is assumed to hold one.
        assert!(journal_holds_access_app(&serde_json::json!({"x": 1}), 4));
    }

    #[tokio::test]
    async fn a_token_a_run_still_needs_for_undo_is_not_deleted() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![token_row(4)]])
            .append_query_results([count(0)])
            .append_query_results([count(0)])
            .append_query_results([vec![
                run_row(7, &[app_entry(4, JournalState::Done)]),
                run_row(8, &[app_entry(4, JournalState::Absent)]),
            ]])
            .into_connection();
        let result = service(db).delete(4).await;
        match result {
            Err(error @ AccessProviderError::NeededToUndo { .. }) => {
                assert!(matches!(
                    &error,
                    AccessProviderError::NeededToUndo { provider_id: 4, runs } if runs == &vec![7]
                ));
                assert!(error.to_string().contains("undo run(s) 7"));
            }
            other => panic!("expected NeededToUndo, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_token_whose_apps_are_all_undone_is_deleted() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![token_row(4)]])
            .append_query_results([count(0)])
            .append_query_results([count(0)])
            .append_query_results([vec![run_row(8, &[app_entry(4, JournalState::Absent)])]])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        assert_eq!(service(db).delete(4).await.expect("deleted").id, 4);
    }

    #[tokio::test]
    async fn a_token_that_protects_an_address_is_not_deleted() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![token_row(4)]])
            .append_query_results([count(1)])
            .append_query_results([count(0)])
            .into_connection();
        let result = service(db).delete(4).await;
        assert!(matches!(
            result,
            Err(AccessProviderError::InUse {
                provider_id: 4,
                addresses: 1
            })
        ));
    }

    #[tokio::test]
    async fn an_unknown_token_is_not_found() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([Vec::<cloudflare_access_providers::Model>::new()])
            .into_connection();
        assert!(matches!(
            service(db).credentials(9).await,
            Err(AccessProviderError::NotFound { provider_id: 9 })
        ));
    }
}
