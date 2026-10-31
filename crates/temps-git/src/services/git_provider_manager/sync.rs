// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Repository sync for a connection: the background task, its `syncing` flag
//! guard and hard deadline, and the provider listing it persists.

use super::*;

// Create JWT token for authentication
use octocrab::models::{AppId, InstallationId, InstallationToken};
use octocrab::params::apps::CreateInstallationAccessToken;
use octocrab::Octocrab;

use crate::services::github::create_github_app_jwt;

/// Hard ceiling on a single sync run. If we haven't finished within this
/// window something has gone badly wrong (stuck HTTPS connection, provider
/// outage with retries spinning, etc) and we'd rather abort and release
/// the `syncing` flag than hold it indefinitely. 30 minutes is well above
/// a healthy 20,000-repo sync's wall time and well below "the user has
/// given up and restarted the server."
const SYNC_HARD_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// Drop guard that resets `syncing=false` on a connection when it leaves
/// scope. Covers every exit path — Ok, Err, panic, task cancel, deadline
/// timeout — so the flag never gets stuck in the `true` state.
///
/// The guard owns an `Arc<GitProviderManager>` so the write is issued
/// against the live DB handle. Because `Drop` is synchronous, we spawn a
/// detached tokio task to perform the update. If the runtime is shutting
/// down and can't accept new tasks we log and move on — the startup
/// reset in `plugin.rs` will clean up on the next boot.
struct SyncFlagGuard {
    manager: Option<Arc<GitProviderManager>>,
    connection_id: i32,
}

impl SyncFlagGuard {
    fn new(manager: Arc<GitProviderManager>, connection_id: i32) -> Self {
        Self {
            manager: Some(manager),
            connection_id,
        }
    }
}

impl Drop for SyncFlagGuard {
    fn drop(&mut self) {
        let Some(manager) = self.manager.take() else {
            return;
        };
        let connection_id = self.connection_id;
        // Best-effort: if we're mid-tokio-shutdown, `spawn` will panic.
        // Catch that so a panicking Drop doesn't bring down the process.
        let spawn_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tokio::spawn(async move {
                if let Err(e) = manager
                    .set_connection_syncing_status(connection_id, false)
                    .await
                {
                    tracing::error!(
                        connection_id,
                        error = %e,
                        "Failed to reset syncing flag from SyncFlagGuard"
                    );
                }
            });
        }));
        if spawn_result.is_err() {
            tracing::warn!(
                connection_id,
                "Could not spawn syncing-flag reset during Drop (runtime likely shutting down); startup reset will handle it"
            );
        }
    }
}

impl GitProviderManager {
    /// Set syncing status for a connection. When flipping to `true`, we also
    /// reset `synced_repository_count` to 0 so the UI's running progress
    /// starts fresh for this sync.
    async fn set_connection_syncing_status(
        &self,
        connection_id: i32,
        syncing: bool,
    ) -> Result<(), GitProviderManagerError> {
        let connection = self.get_connection(connection_id).await?;
        let mut active_model: git_provider_connections::ActiveModel = connection.into();
        active_model.syncing = Set(syncing);
        if syncing {
            active_model.synced_repository_count = Set(0);
        }
        active_model.update(self.db.as_ref()).await?;
        Ok(())
    }

    /// Sync repositories from a connection
    /// Mark a connection as syncing and spawn the sync loop as a detached
    /// background task.
    ///
    /// Returns as soon as `syncing=true` has been persisted, so the HTTP
    /// caller can close the connection without interrupting the sync. The
    /// spawned task owns a drop-guard that flips `syncing=false` on *any*
    /// exit path — normal completion, error, timeout, or drop. The whole
    /// inner sync is wrapped in a generous deadline to guarantee the flag
    /// cannot get stuck forever even if the provider hangs mid-request.
    ///
    /// Returns `SyncInProgress` if the connection is already syncing.
    pub async fn spawn_sync_repositories(
        self: Arc<Self>,
        connection_id: i32,
    ) -> Result<(), GitProviderManagerError> {
        let connection = self.get_connection(connection_id).await?;
        if connection.syncing {
            return Err(GitProviderManagerError::SyncInProgress);
        }

        // Flip to syncing=true before returning so the client's immediate
        // follow-up read observes the fresh state.
        self.set_connection_syncing_status(connection_id, true)
            .await?;

        let manager = self.clone();
        tokio::spawn(async move {
            manager.run_sync_guarded(connection_id).await;
        });

        Ok(())
    }

    /// Synchronous variant of `spawn_sync_repositories` for callers that
    /// need to block until the sync finishes (tests, CLI flows). Everyday
    /// code paths should prefer `spawn_sync_repositories` so the sync
    /// survives HTTP client disconnects.
    pub async fn sync_repositories(
        &self,
        connection_id: i32,
    ) -> Result<Vec<repositories::Model>, GitProviderManagerError> {
        let connection = self.get_connection(connection_id).await?;
        if connection.syncing {
            return Err(GitProviderManagerError::SyncInProgress);
        }

        self.set_connection_syncing_status(connection_id, true)
            .await?;

        // Hold the guard on the stack so even a panic inside the inner
        // call unwinds through the Drop impl and resets the flag.
        let guard = SyncFlagGuard::new(self.clone_arc(), connection_id);
        let result = tokio::time::timeout(
            SYNC_HARD_DEADLINE,
            self.sync_repositories_internal(connection_id),
        )
        .await;
        drop(guard);

        match result {
            Ok(Ok(())) => {
                let repos = repositories::Entity::find()
                    .filter(repositories::Column::GitProviderConnectionId.eq(connection_id))
                    .order_by_desc(repositories::Column::PushedAt)
                    .all(self.db.as_ref())
                    .await?;
                Ok(repos)
            }
            Ok(Err(e)) => {
                error!(
                    connection_id,
                    error = %e,
                    "Repository sync failed"
                );
                Err(e)
            }
            Err(_elapsed) => {
                error!(
                    connection_id,
                    deadline_secs = SYNC_HARD_DEADLINE.as_secs(),
                    "Repository sync exceeded hard deadline and was aborted"
                );
                Err(GitProviderManagerError::SyncTimeout {
                    connection_id,
                    deadline_secs: SYNC_HARD_DEADLINE.as_secs(),
                })
            }
        }
    }

    /// Runs the inner sync loop behind a drop-guard and a hard deadline.
    /// Meant to be invoked from a detached `tokio::spawn` — never returns
    /// errors to a caller, only logs them.
    async fn run_sync_guarded(self: Arc<Self>, connection_id: i32) {
        let guard = SyncFlagGuard::new(self.clone(), connection_id);
        let outcome = tokio::time::timeout(
            SYNC_HARD_DEADLINE,
            self.sync_repositories_internal(connection_id),
        )
        .await;

        let failure = match &outcome {
            Ok(Ok(())) => {
                tracing::info!(connection_id, "Repository sync completed");
                None
            }
            Ok(Err(e)) => {
                tracing::error!(
                    connection_id,
                    error = %e,
                    "Repository sync failed; syncing flag has been reset"
                );
                Some(e.to_string())
            }
            Err(_elapsed) => {
                tracing::error!(
                    connection_id,
                    deadline_secs = SYNC_HARD_DEADLINE.as_secs(),
                    "Repository sync exceeded hard deadline and was aborted"
                );
                Some(format!(
                    "Repository sync exceeded the {}s deadline and was aborted",
                    SYNC_HARD_DEADLINE.as_secs()
                ))
            }
        };

        // Record the outcome BEFORE the guard releases `syncing`: a client
        // that sees `syncing = false` must already be able to read how the
        // sync ended, or a failure would briefly look like a success.
        if let Err(e) = self.record_sync_outcome(connection_id, failure).await {
            tracing::error!(
                connection_id,
                error = %e,
                "Failed to record repository sync outcome"
            );
        }
        drop(guard);
    }

    /// Persist how a sync ended: the failure reason and when it happened, or
    /// cleared on success so a stale error never outlives a working sync.
    async fn record_sync_outcome(
        &self,
        connection_id: i32,
        failure: Option<String>,
    ) -> Result<(), GitProviderManagerError> {
        let failed_at = failure.as_ref().map(|_| chrono::Utc::now());
        git_provider_connections::Entity::update_many()
            .col_expr(
                git_provider_connections::Column::LastSyncError,
                sea_orm::sea_query::Expr::value(failure),
            )
            .col_expr(
                git_provider_connections::Column::LastSyncErrorAt,
                sea_orm::sea_query::Expr::value(failed_at),
            )
            .filter(git_provider_connections::Column::Id.eq(connection_id))
            .exec(self.db.as_ref())
            .await?;
        Ok(())
    }

    /// Wrap `&self` in an `Arc` copy of the manager. Used to hand the
    /// manager into a `Drop` guard that needs owned access.
    fn clone_arc(&self) -> Arc<Self> {
        Arc::new(self.clone())
    }

    async fn sync_repositories_internal(
        &self,
        connection_id: i32,
    ) -> Result<(), GitProviderManagerError> {
        let connection = self.get_connection(connection_id).await?;
        let provider = self.get_provider(connection.provider_id).await?;
        let provider_service = self.get_provider_service(connection.provider_id).await?;

        // Get the access token - for GitHub Apps, generate an installation token
        let access_token = if provider.provider_type == "github"
            && provider.auth_method == "github_app"
        {
            // For GitHub Apps, we need to generate an installation access token
            tracing::info!(
                "Generating installation access token for GitHub App connection {}",
                connection_id
            );

            // Get the installation ID from the connection
            let installation_id = connection
                .installation_id
                .as_ref()
                .and_then(|id| id.parse::<i64>().ok())
                .ok_or_else(|| {
                    GitProviderManagerError::InvalidConfiguration(
                        "GitHub App connection missing valid installation ID".to_string(),
                    )
                })?;

            // Get the GitHub App credentials from the provider's auth_config
            let app_id = provider
                .auth_config
                .get("app_id")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| {
                    GitProviderManagerError::InvalidConfiguration(
                        "GitHub App missing app_id".to_string(),
                    )
                })?;

            let private_key = provider
                .auth_config
                .get("private_key")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    GitProviderManagerError::InvalidConfiguration(
                        "GitHub App missing private_key".to_string(),
                    )
                })?;

            // Decrypt the private key if it's encrypted
            let decrypted_private_key = if private_key.starts_with("enc:") {
                self.decrypt_string(private_key).await?
            } else {
                private_key.to_string()
            };

            let key = jsonwebtoken::EncodingKey::from_rsa_pem(decrypted_private_key.as_bytes())
                .map_err(|e| {
                    GitProviderManagerError::InvalidConfiguration(format!(
                        "Invalid private key: {}",
                        e
                    ))
                })?;

            let app_id_param = AppId(app_id as u64);
            let jwt_token = create_github_app_jwt(app_id_param.0, &key).map_err(|e| {
                GitProviderManagerError::InvalidConfiguration(format!(
                    "Failed to create JWT: {}",
                    e
                ))
            })?;

            let octocrab = Octocrab::builder()
                .personal_token(jwt_token)
                .build()
                .map_err(|e| {
                    GitProviderManagerError::InvalidConfiguration(format!(
                        "Failed to build Octocrab client: {}",
                        e
                    ))
                })?;

            // Get the installation
            let installation = octocrab
                .apps()
                .installation(InstallationId(installation_id as u64))
                .await
                .map_err(|e| {
                    GitProviderManagerError::InvalidConfiguration(format!(
                        "Failed to get installation: {}",
                        e
                    ))
                })?;

            // Create an installation access token
            let create_access_token = CreateInstallationAccessToken::default();
            let gh_access_tokens_url =
                Url::parse(installation.access_tokens_url.as_ref().ok_or_else(|| {
                    GitProviderManagerError::InvalidConfiguration(
                        "Missing access tokens URL".to_string(),
                    )
                })?)
                .map_err(|e| {
                    GitProviderManagerError::InvalidConfiguration(format!("Invalid URL: {}", e))
                })?;

            let access: InstallationToken = octocrab
                .post(gh_access_tokens_url.path(), Some(&create_access_token))
                .await
                .map_err(|e| {
                    GitProviderManagerError::InvalidConfiguration(format!(
                        "Failed to create installation token: {}",
                        e
                    ))
                })?;

            // Update the connection with the new token
            if let Some(expires_at) = &access.expires_at {
                let expires_at = chrono::DateTime::parse_from_rfc3339(expires_at).map_err(|e| {
                    GitProviderManagerError::InvalidConfiguration(format!(
                        "Invalid expiration date: {}",
                        e
                    ))
                })?;
                let expires_at = expires_at.with_timezone(&chrono::Utc);

                self.update_connection_tokens(
                    connection.id,
                    access.token.clone(),
                    None,
                    Some(expires_at),
                )
                .await?;
            }

            access.token
        } else {
            // For non-GitHub App providers, we'll use execute_with_refresh below
            // which handles token decryption and refresh automatically
            String::new() // Placeholder, won't be used directly for OAuth connections
        };

        // Fetch repositories from provider
        // Only pass organization name if account_type is "Organization"
        let organization = if connection.account_type == "Organization" {
            Some(connection.account_name.clone())
        } else {
            None
        };

        // Streaming sync: we flush each page to the database as soon as it
        // arrives from the provider. This matters for tenants with thousands
        // of repos: buffering everything in memory before writing was the old
        // design and it doesn't scale past ~a few thousand projects, plus a
        // failure on page N threw away pages 1..N-1 of work.
        //
        // Deletion detection still works: we track every full_name we've seen
        // across pages, then at the end any existing row whose full_name
        // wasn't seen is considered stale. We only run that sweep on clean
        // completion — a partial sync must NOT delete rows we simply haven't
        // observed yet.
        let mut seen_full_names: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut total_inserted: usize = 0;
        let mut total_updated: usize = 0;
        let mut page: u32 = 1;
        // Hard ceiling so a broken cursor can't loop forever. 200 pages of
        // 100 repos = 20,000 — enough for realistic syncs; if you need more,
        // bump it here.
        const MAX_PAGES: u32 = 200;

        let is_github_app =
            provider.provider_type == "github" && provider.auth_method == "github_app";

        loop {
            // Fetch one page. For GitHub Apps we already have a fresh
            // installation token; for OAuth we route through
            // execute_with_refresh so the token refreshes mid-sync if needed.
            let page_result = if is_github_app {
                provider_service
                    .list_repositories_page(&access_token, organization.as_deref(), page)
                    .await?
            } else {
                self.execute_with_refresh(connection_id, |token| {
                    let org = organization.clone();
                    let svc = provider_service.clone();
                    async move {
                        svc.list_repositories_page(&token, org.as_deref(), page)
                            .await
                    }
                })
                .await?
            };

            let page_items = page_result.items;
            let next_page = page_result.next_page;

            if page_items.is_empty() && page == 1 {
                tracing::info!(
                    "Sync for connection {} returned zero repositories on page 1",
                    connection_id
                );
            }

            if !page_items.is_empty() {
                // Flush this page: diff against existing rows and upsert.
                // Each page is an independent transaction from the DB's POV —
                // if page N+1 fails, pages 1..N are already persisted.
                let full_names: Vec<String> =
                    page_items.iter().map(|r| r.full_name.clone()).collect();

                let existing_rows = repositories::Entity::find()
                    .filter(repositories::Column::GitProviderConnectionId.eq(connection_id))
                    .filter(repositories::Column::FullName.is_in(full_names.clone()))
                    .all(self.db.as_ref())
                    .await?;

                let mut existing_map: std::collections::HashMap<String, repositories::Model> =
                    existing_rows
                        .into_iter()
                        .map(|r| (r.full_name.clone(), r))
                        .collect();

                let mut to_insert: Vec<repositories::ActiveModel> = Vec::new();
                let mut to_update: Vec<repositories::ActiveModel> = Vec::new();

                for repo in page_items {
                    seen_full_names.insert(repo.full_name.clone());
                    if let Some(existing) = existing_map.remove(&repo.full_name) {
                        let mut active: repositories::ActiveModel = existing.into();
                        active.description = Set(repo.description.clone());
                        active.default_branch = Set(repo.default_branch.clone());
                        active.language = Set(repo.language.clone());
                        active.size = Set(repo.size as i32);
                        active.stargazers_count = Set(repo.stars);
                        active.pushed_at = Set(repo.pushed_at.unwrap_or(repo.updated_at));
                        active.clone_url = Set(Some(repo.clone_url.clone()));
                        active.ssh_url = Set(Some(repo.ssh_url.clone()));
                        active.created_at = Set(repo.created_at);
                        active.updated_at = Set(repo.updated_at);
                        to_update.push(active);
                    } else {
                        let new_row = repositories::ActiveModel {
                            git_provider_connection_id: Set(connection_id),
                            owner: Set(repo.owner.clone()),
                            name: Set(repo.name.clone()),
                            full_name: Set(repo.full_name.clone()),
                            description: Set(repo.description.clone()),
                            private: Set(repo.private),
                            fork: Set(false),
                            size: Set(repo.size as i32),
                            stargazers_count: Set(repo.stars),
                            watchers_count: Set(repo.forks),
                            language: Set(repo.language.clone()),
                            default_branch: Set(repo.default_branch.clone()),
                            open_issues_count: Set(0),
                            topics: Set("".to_string()),
                            repo_object: Set(json!(repo).to_string()),
                            created_at: Set(repo.created_at),
                            updated_at: Set(repo.updated_at),
                            pushed_at: Set(repo.pushed_at.unwrap_or(repo.updated_at)),
                            clone_url: Set(Some(repo.clone_url.clone())),
                            ssh_url: Set(Some(repo.ssh_url.clone())),
                            installation_id: Set(None),
                            preset: Set(None),
                            ..Default::default()
                        };
                        to_insert.push(new_row);
                    }
                }

                let page_update_count = to_update.len();
                let page_insert_count = to_insert.len();

                for row in to_update {
                    row.update(self.db.as_ref()).await?;
                }
                if !to_insert.is_empty() {
                    repositories::Entity::insert_many(to_insert)
                        .exec(self.db.as_ref())
                        .await?;
                }

                total_updated += page_update_count;
                total_inserted += page_insert_count;

                tracing::info!(
                    connection_id,
                    page,
                    updates = page_update_count,
                    inserts = page_insert_count,
                    running_total_updated = total_updated,
                    running_total_inserted = total_inserted,
                    "Flushed repository sync page"
                );

                // Touch last_synced_at and the running progress count on
                // every page so the UI can show a long-running sync
                // advancing — a stale "last synced 1 day ago" on a
                // 20,000-repo tenant is worse than a frequently-bumped
                // timestamp, and the count lets the UI display
                // "Synced 1,400 repos…" live.
                let touch = git_provider_connections::ActiveModel {
                    id: Set(connection_id),
                    last_synced_at: Set(Some(chrono::Utc::now())),
                    synced_repository_count: Set((total_inserted + total_updated) as i32),
                    ..Default::default()
                };
                touch.update(self.db.as_ref()).await?;
            }

            match next_page {
                Some(next) if next > page && page < MAX_PAGES => page = next,
                _ => break,
            }
        }

        tracing::info!(
            connection_id,
            total_updated,
            total_inserted,
            pages_scanned = page,
            "Sync complete"
        );

        // Deletion sweep: any existing row we didn't see across ALL pages is
        // stale (repo deleted/transferred/unshared on the provider). We only
        // run this after a clean completion — if the loop errored out, we'd
        // delete rows we just hadn't fetched yet, which would be worse than a
        // stale row.
        if !seen_full_names.is_empty() {
            let existing_full_names: Vec<String> = repositories::Entity::find()
                .select_only()
                .column(repositories::Column::FullName)
                .filter(repositories::Column::GitProviderConnectionId.eq(connection_id))
                .into_tuple::<String>()
                .all(self.db.as_ref())
                .await?;

            let stale: Vec<String> = existing_full_names
                .into_iter()
                .filter(|name| !seen_full_names.contains(name))
                .collect();

            if !stale.is_empty() {
                tracing::info!(
                    connection_id,
                    stale_count = stale.len(),
                    "Deleting repositories no longer present upstream"
                );
                repositories::Entity::delete_many()
                    .filter(repositories::Column::GitProviderConnectionId.eq(connection_id))
                    .filter(repositories::Column::FullName.is_in(stale))
                    .exec(self.db.as_ref())
                    .await?;
            }
        }

        // Final last_synced_at and progress-count update (redundant with
        // per-page touches but makes the completion instant precise and
        // guarantees the count matches the exact final total).
        let final_touch = git_provider_connections::ActiveModel {
            id: Set(connection_id),
            last_synced_at: Set(Some(chrono::Utc::now())),
            synced_repository_count: Set((total_inserted + total_updated) as i32),
            ..Default::default()
        };
        final_touch.update(self.db.as_ref()).await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::git_provider_manager::test_support::*;
    use temps_database::test_utils::TestDatabase;

    /// A sync runs detached, so its failure used to reach only the server log:
    /// the console saw `syncing` flip back to false and could not tell a failed
    /// sync from one that succeeded and found nothing. The reason must be
    /// persisted before `syncing` is released, and a later success clears it.
    #[tokio::test]
    async fn failed_sync_is_recorded_on_the_connection_and_cleared_on_success() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();

        // An auth config that doesn't deserialize makes the sync fail before
        // any provider request, exactly like a misconfigured provider does.
        let provider = git_providers::ActiveModel {
            name: Set("broken-provider".to_string()),
            provider_type: Set("github".to_string()),
            base_url: Set(None),
            api_url: Set(None),
            auth_method: Set("pat".to_string()),
            auth_config: Set(serde_json::json!({})),
            webhook_secret: Set(None),
            is_active: Set(true),
            is_default: Set(false),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        let connection = git_provider_connections::ActiveModel {
            provider_id: Set(provider.id),
            user_id: Set(None),
            account_name: Set("sync-account".to_string()),
            account_type: Set("User".to_string()),
            access_token: Set(None),
            refresh_token: Set(None),
            token_expires_at: Set(None),
            refresh_token_expires_at: Set(None),
            installation_id: Set(None),
            metadata: Set(None),
            is_active: Set(true),
            is_expired: Set(false),
            syncing: Set(true),
            last_synced_at: Set(None),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        let encryption_service = Arc::new(
            temps_core::EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .unwrap(),
        );
        let manager = Arc::new(GitProviderManager::new(
            db.clone(),
            encryption_service,
            Arc::new(MockJobQueue) as Arc<dyn JobQueue>,
            create_test_config_service(db.clone()),
        ));

        manager.clone().run_sync_guarded(connection.id).await;

        let failed = manager.get_connection(connection.id).await.unwrap();
        let reason = failed
            .last_sync_error
            .as_deref()
            .expect("a failed sync must record why it failed");
        assert!(!reason.is_empty());
        assert!(failed.last_sync_error_at.is_some());

        // The guard releases `syncing` from a spawned task; give it a moment.
        let mut released = false;
        for _ in 0..50 {
            if !manager.get_connection(connection.id).await.unwrap().syncing {
                released = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            released,
            "a failed sync must still release the syncing flag"
        );

        manager
            .record_sync_outcome(connection.id, None)
            .await
            .unwrap();
        let recovered = manager.get_connection(connection.id).await.unwrap();
        assert_eq!(recovered.last_sync_error, None);
        assert_eq!(recovered.last_sync_error_at, None);
    }
}
