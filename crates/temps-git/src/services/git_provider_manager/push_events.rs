// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Push events: authenticate repository webhooks and queue a `GitPushEventJob`
//! for every project that should act on the push.

use super::*;

impl GitProviderManager {
    /// Handle push event by queueing GitPushEventJob for all matching projects
    pub async fn handle_push_event(
        &self,
        owner: String,
        repo: String,
        branch: Option<String>,
        tag: Option<String>,
        commit: String,
    ) -> Result<(), GitProviderManagerError> {
        // Branch/tag deletions send the all-zeros "null SHA" in the `after`
        // field (GitHub, GitLab, and others all follow this Git convention).
        // There is no commit to deploy, so creating a deployment from it would
        // queue a download_repo job that fails with
        // "Failed to checkout ref 0000000...". Ignore these events.
        if commit.is_empty() || commit.chars().all(|c| c == '0') {
            tracing::info!(
                "Ignoring push event for {}/{} with null SHA (branch/tag deletion), branch: {:?}, tag: {:?}",
                owner,
                repo,
                branch,
                tag
            );
            return Ok(());
        }

        // Find all projects with this owner and repo
        let matching_projects = projects::Entity::find()
            .filter(projects::Column::RepoOwner.eq(&owner))
            .filter(projects::Column::RepoName.eq(&repo))
            .all(self.db.as_ref())
            .await
            .map_err(|e| {
                tracing::error!("Failed to find projects for {}/{}: {}", owner, repo, e);
                GitProviderManagerError::DatabaseError(e)
            })?;

        self.queue_push_event_for_projects(branch, tag, commit, matching_projects)
            .await
            .map(|_| ())
    }

    /// Handle an authenticated push event for a pre-authorized set of projects.
    ///
    /// Secret-in-path webhook handlers must use this method after token lookup so
    /// the authenticated token owner, not untrusted payload repository fields,
    /// determines which projects can receive deployment jobs.
    pub async fn handle_push_event_for_projects(
        &self,
        branch: Option<String>,
        tag: Option<String>,
        commit: String,
        projects: Vec<projects::Model>,
    ) -> Result<usize, GitProviderManagerError> {
        self.queue_push_event_for_projects(branch, tag, commit, projects)
            .await
    }

    /// Projects whose own GitHub repository webhook signed this delivery.
    ///
    /// A GitHub connection that is not a GitHub App (personal access token or
    /// OAuth) has no App webhook to deliver pushes, so Temps registers a
    /// repository webhook per project, each signed with a secret of its own.
    /// Only projects whose stored secret verifies `signature`
    /// (`X-Hub-Signature-256`) over the raw `body` are returned. Several
    /// projects may point at the same repository, each with its own hook, and
    /// a delivery signed with one project's secret must not deploy the others.
    pub async fn github_repo_webhook_projects(
        &self,
        owner: &str,
        repo: &str,
        body: &[u8],
        signature: &str,
    ) -> Result<Vec<projects::Model>, GitProviderManagerError> {
        let candidates = projects::Entity::find()
            .filter(projects::Column::RepoOwner.eq(owner))
            .filter(projects::Column::RepoName.eq(repo))
            .filter(projects::Column::GithubWebhookSigningToken.is_not_null())
            .all(self.db.as_ref())
            .await?;

        let mut authorized = Vec::with_capacity(candidates.len());
        for project in candidates {
            let Some(encrypted) = project.github_webhook_signing_token.as_deref() else {
                continue;
            };
            let secret = match self.decrypt_string(encrypted).await {
                Ok(secret) => secret,
                Err(e) => {
                    warn!(
                        "Failed to decrypt GitHub webhook secret for project {} ({}/{}): {}",
                        project.id, owner, repo, e
                    );
                    continue;
                }
            };
            match crate::services::github_provider::github_webhook_signature_matches(
                body, signature, &secret,
            ) {
                Ok(true) => authorized.push(project),
                Ok(false) => debug!(
                    "GitHub repository webhook signature does not match project {} ({}/{})",
                    project.id, owner, repo
                ),
                Err(e) => warn!(
                    "Cannot verify GitHub repository webhook for project {} ({}/{}): {}",
                    project.id, owner, repo, e
                ),
            }
        }

        Ok(authorized)
    }

    /// Whether a project with this source type should still act on a Git push.
    ///
    /// `Git` obviously does. `Manual` does too — it is documented as "accepts
    /// any deployment method... allows switching between deployment methods
    /// without recreating the project", so a repository left configured on a
    /// manual project is a deliberate, still-live Git source, not a leftover.
    ///
    /// The rest (`DockerImage`, `StaticFiles`, `UploadedSource`) named a
    /// non-Git source explicitly. A push to their old repository has nothing
    /// to build: the planner fails, but only after the deployment row exists
    /// and any in-flight deployment has been cancelled.
    fn project_accepts_git_push(source_type: &temps_entities::source_type::SourceType) -> bool {
        use temps_entities::source_type::SourceType;
        matches!(source_type, SourceType::Git | SourceType::Manual)
    }

    async fn queue_push_event_for_projects(
        &self,
        branch: Option<String>,
        tag: Option<String>,
        commit: String,
        matching_projects: Vec<projects::Model>,
    ) -> Result<usize, GitProviderManagerError> {
        // Branch/tag deletions send the all-zeros "null SHA" in the `after`
        // field. There is no commit to deploy, so skip them before queueing.
        if commit.is_empty() || commit.chars().all(|c| c == '0') {
            tracing::info!(
                "Ignoring push event with null SHA (branch/tag deletion), branch: {:?}, tag: {:?}",
                branch,
                tag
            );
            return Ok(0);
        }

        // A project that has been switched away from Git keeps its
        // repo_owner/repo_name so the operator can switch back in one click,
        // but it must stop acting on pushes. Webhooks match on repository
        // alone, so without this a push to the old repository still reaches a
        // `manual` project — where the planner treats the retained repo fields
        // as a Git source and runs a real build with the project's deployment
        // secrets — and still creates failed deployments (cancelling in-flight
        // ones) for docker_image/static_files projects.
        let matching_projects: Vec<projects::Model> = matching_projects
            .into_iter()
            .filter(|project| {
                let accepts = Self::project_accepts_git_push(&project.source_type);
                if !accepts {
                    tracing::info!(
                        project_id = project.id,
                        source_type = ?project.source_type,
                        "Ignoring push event for project that is no longer Git-sourced"
                    );
                }
                accepts
            })
            .collect();

        if matching_projects.is_empty() {
            tracing::warn!("No authorized projects found, skipping push event");
            return Ok(0);
        }

        tracing::info!(
            "Found {} authorized projects, queueing push events",
            matching_projects.len()
        );

        let triggered = matching_projects.len();

        // Queue a GitPushEventJob for each project
        for project in matching_projects {
            let owner = project.repo_owner.clone();
            let repo = project.repo_name.clone();
            let push_job = temps_core::GitPushEventJob {
                owner: owner.clone(),
                repo: repo.clone(),
                branch: branch.clone(),
                tag: tag.clone(),
                commit: commit.clone(),
                project_id: project.id,
                // Real git provider webhook — honour automatic_deploy.
                manual_trigger: false,
                rollback_from_deployment_id: None,
                // Webhook: infer the target environment(s) from the branch.
                target_environment_id: None,
                recovery_of_deployment_id: None,
            };

            if let Err(e) = self
                .queue_service
                .send(temps_core::Job::GitPushEvent(push_job))
                .await
            {
                tracing::error!(
                    "Failed to queue push event job for project {}: {}",
                    project.id,
                    e
                );
                // Continue with other projects even if one fails
            } else {
                tracing::info!(
                    "Queued push event for project {} ({}/{})",
                    project.id,
                    owner,
                    repo
                );
            }
        }

        Ok(triggered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::git_provider_manager::test_support::*;
    use temps_core::{async_trait::async_trait, Job, JobReceiver, QueueError};

    use temps_entities::source_type::SourceType;

    /// A project switched away from Git keeps its repo fields so the operator
    /// can switch back in one click, but webhooks match on repository alone —
    /// so a push to the old repository must not queue a deployment for it.
    #[test]
    fn only_git_capable_projects_act_on_push_events() {
        assert!(super::GitProviderManager::project_accepts_git_push(
            &SourceType::Git
        ));
        // Manual explicitly accepts any deployment method, including Git.
        assert!(super::GitProviderManager::project_accepts_git_push(
            &SourceType::Manual
        ));
        for source_type in [
            SourceType::DockerImage,
            SourceType::StaticFiles,
            SourceType::UploadedSource,
        ] {
            assert!(
                !super::GitProviderManager::project_accepts_git_push(&source_type),
                "{source_type} must ignore Git pushes"
            );
        }
    }

    /// A queue that fails the test if anything is ever enqueued. Used to assert
    /// that a push event was short-circuited before any deployment job was sent.
    struct PanicOnSendJobQueue;

    #[async_trait]
    impl JobQueue for PanicOnSendJobQueue {
        async fn send(&self, job: Job) -> Result<(), QueueError> {
            panic!("no job should be queued, but got: {job:?}");
        }

        fn subscribe(&self) -> Box<dyn JobReceiver> {
            panic!("subscribe not needed for tests")
        }
    }

    #[tokio::test]
    async fn handle_push_event_ignores_null_sha_deletion() {
        use sea_orm::{DatabaseBackend, MockDatabase};

        // A MockDatabase with no queued results errors on the first query —
        // proving the null-SHA guard returns before touching the database. The
        // panic-on-send queue proves no deployment job is enqueued.
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());

        let encryption_service = Arc::new(
            temps_core::EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .unwrap(),
        );
        let queue_service = Arc::new(PanicOnSendJobQueue) as Arc<dyn JobQueue>;
        let config_service = create_test_config_service(db.clone());

        let manager =
            GitProviderManager::new(db, encryption_service, queue_service, config_service);

        // Full all-zeros null SHA (GitHub/GitLab branch deletion).
        let result = manager
            .handle_push_event(
                "owner".into(),
                "repo".into(),
                Some("deleted-branch".into()),
                None,
                "0000000000000000000000000000000000000000".into(),
            )
            .await;
        assert!(
            result.is_ok(),
            "null-SHA push event should be ignored: {result:?}"
        );

        // Empty commit string is treated the same way.
        let result = manager
            .handle_push_event(
                "owner".into(),
                "repo".into(),
                Some("b".into()),
                None,
                String::new(),
            )
            .await;
        assert!(
            result.is_ok(),
            "empty-SHA push event should be ignored: {result:?}"
        );
    }

    /// Records every job it is sent.
    #[derive(Default)]
    struct RecordingJobQueue {
        jobs: std::sync::Mutex<Vec<Job>>,
    }

    #[async_trait]
    impl JobQueue for RecordingJobQueue {
        async fn send(&self, job: Job) -> Result<(), QueueError> {
            self.jobs.lock().unwrap().push(job);
            Ok(())
        }

        fn subscribe(&self) -> Box<dyn JobReceiver> {
            panic!("subscribe not needed for tests")
        }
    }

    fn github_repo_project(id: i32, encrypted_secret: Option<String>) -> projects::Model {
        let now = chrono::Utc::now();
        projects::Model {
            id,
            name: format!("project-{id}"),
            repo_name: "example-repository".to_string(),
            repo_owner: "example-owner".to_string(),
            directory: "/".to_string(),
            pull_only_root_directory: false,
            main_branch: "main".to_string(),
            preset: temps_entities::preset::Preset::Vite,
            preset_config: None,
            deployment_config: None,
            created_at: now,
            updated_at: now,
            slug: format!("project-{id}"),
            template_slug: None,
            is_deleted: false,
            deleted_at: None,
            last_deployment: None,
            is_public_repo: false,
            git_url: None,
            git_provider_connection_id: Some(7),
            attack_mode: false,
            shield_mode: "log".to_string(),
            ai_alert_summaries_enabled: None,
            ai_api_traffic_summary_enabled: None,
            allow_alternate_sources: None,
            hosting: Default::default(),
            ai_debug_chat_enabled: None,
            ai_write_actions_enabled: false,
            cross_project_trace_sharing: true,
            serves_own_temps_api: false,
            error_source_context_enabled: false,
            vulnerability_scanning_enabled: false,
            error_source_root: None,
            image_retention_hours: None,
            cloud_telemetry_fidelity:
                temps_entities::cloud_telemetry_fidelity::CloudTelemetryFidelity::Metered,
            cloud_telemetry_write_mode:
                temps_entities::cloud_telemetry_write_mode::CloudTelemetryWriteMode::Local,
            cloud_analytics_write_mode:
                temps_entities::cloud_analytics_write_mode::CloudAnalyticsWriteMode::Local,
            instantiation_token: None,
            cloud_telemetry_attribute_allowlist: Vec::new(),
            enable_preview_environments: false,
            preview_envs_on_demand: false,
            preview_envs_idle_timeout_seconds: 300,
            preview_envs_wake_timeout_seconds: 30,
            source_type: temps_entities::source_type::SourceType::Git,
            project_type: temps_entities::types::ProjectType::Server,
            service_template: None,
            gitlab_webhook_id: None,
            gitlab_webhook_signing_token: None,
            gitea_webhook_signing_token: None,
            github_webhook_id: encrypted_secret.as_ref().map(|_| 1000 + i64::from(id)),
            github_webhook_signing_token: encrypted_secret,
            bitbucket_webhook_token: None,
            bitbucket_webhook_hook_id: None,
            generic_webhook_token: None,
            log_ingest_lines_per_sec: None,
            log_ingest_kib_per_sec: None,
        }
    }

    fn github_signature(secret: &str, body: &[u8]) -> String {
        use hmac::{Hmac, KeyInit, Mac};
        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    /// #1596: a push to a repository connected through a GitHub personal
    /// access token never deployed, because only the GitHub App webhook was
    /// accepted. A repository webhook delivery signed with the project's own
    /// secret must now queue a deployment for that project -- and only that
    /// one, when another project on the same repository has its own secret.
    #[tokio::test]
    async fn github_repo_webhook_push_deploys_only_projects_whose_secret_signed_it() {
        use sea_orm::{DatabaseBackend, MockDatabase};

        let encryption_service = Arc::new(
            temps_core::EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .unwrap(),
        );
        let signing_project = github_repo_project(
            1,
            Some(encryption_service.encrypt_string("secret-one").unwrap()),
        );
        let other_project = github_repo_project(
            2,
            Some(encryption_service.encrypt_string("secret-two").unwrap()),
        );

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![signing_project, other_project]])
                .into_connection(),
        );
        let queue = Arc::new(RecordingJobQueue::default());
        let manager = GitProviderManager::new(
            db.clone(),
            encryption_service,
            queue.clone() as Arc<dyn JobQueue>,
            create_test_config_service(db),
        );

        let body =
            br#"{"ref":"refs/heads/main","after":"0123456789abcdef0123456789abcdef01234567"}"#;
        let authorized = manager
            .github_repo_webhook_projects(
                "example-owner",
                "example-repository",
                body,
                &github_signature("secret-one", body),
            )
            .await
            .unwrap();
        assert_eq!(
            authorized.iter().map(|p| p.id).collect::<Vec<_>>(),
            vec![1],
            "only the project whose secret signed the delivery is authorized"
        );

        let queued = manager
            .handle_push_event_for_projects(
                Some("main".to_string()),
                None,
                "0123456789abcdef0123456789abcdef01234567".to_string(),
                authorized,
            )
            .await
            .unwrap();
        assert_eq!(queued, 1);

        let jobs = queue.jobs.lock().unwrap();
        assert_eq!(jobs.len(), 1);
        match &jobs[0] {
            Job::GitPushEvent(job) => {
                assert_eq!(job.project_id, 1);
                assert_eq!(job.branch.as_deref(), Some("main"));
                assert!(
                    !job.manual_trigger,
                    "a webhook push honours automatic_deploy"
                );
            }
            other => panic!("expected a GitPushEvent job, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn github_repo_webhook_rejects_signatures_from_unknown_secrets() {
        use sea_orm::{DatabaseBackend, MockDatabase};

        let encryption_service = Arc::new(
            temps_core::EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .unwrap(),
        );
        let project = github_repo_project(
            1,
            Some(encryption_service.encrypt_string("project-secret").unwrap()),
        );
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![project]])
                .into_connection(),
        );
        let manager = GitProviderManager::new(
            db.clone(),
            encryption_service,
            Arc::new(PanicOnSendJobQueue) as Arc<dyn JobQueue>,
            create_test_config_service(db),
        );

        let body = br#"{"ref":"refs/heads/main"}"#;
        let authorized = manager
            .github_repo_webhook_projects(
                "example-owner",
                "example-repository",
                body,
                &github_signature("attacker-secret", body),
            )
            .await
            .unwrap();
        assert!(authorized.is_empty());
    }
}
