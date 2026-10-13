// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A project group's repository, for monorepos.
//!
//! The group names a repository and branch once; members that inherit it
//! (typically apps in different directories of that repo) follow it. The
//! group is the source of truth, and every change is written through to each
//! inheriting project's own git settings via
//! [`temps_projects::ProjectService::update_git_settings`], so the deploy
//! pipeline, webhooks and the host-socket guard keep working from the
//! project row exactly as they do for a standalone project. Each member keeps
//! its own directory and preset.

use std::collections::HashSet;
use std::sync::Arc;

use chrono::Utc;
use sea_orm::sea_query::LockType;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QuerySelect, Set,
    TransactionTrait,
};
use serde::Serialize;
use temps_core::docker_socket_grant::DeployCaller;
use temps_entities::{
    git_provider_connections, project_group_members, project_groups, projects, repositories,
    source_type::SourceType,
};
use temps_git::services::public_repo::{
    PublicRepoError, PublicRepoProvider, PublicRepoProviderFactory,
};
use temps_git::GitProviderManager;
use temps_projects::ProjectService;
use utoipa::ToSchema;

use crate::error::ProjectGroupError;
use crate::service::ProjectGroupService;

const MAX_FIELD_LEN: usize = 255;

/// Providers whose public repositories can be read without a connection.
pub const PUBLIC_PROVIDERS: [&str; 2] = ["github", "gitlab"];

/// The repository a group points at, as stored plus what it resolves to.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GroupRepository {
    /// The connection the repository is read through. Null for a public
    /// repository (see `public_provider`), or when the git connection was
    /// deleted; the repository must then be connected again before members
    /// can follow it.
    pub git_provider_connection_id: Option<i32>,
    /// `github` or `gitlab` when the repository is public and read without a
    /// connection. Pushes to a public repository cannot trigger deploys.
    #[schema(example = "github")]
    pub public_provider: Option<String>,
    #[schema(example = "acme")]
    pub repo_owner: String,
    #[schema(example = "shop")]
    pub repo_name: String,
    #[schema(example = "main")]
    pub main_branch: String,
    /// The synced repository record, used to scan a connected repo for apps.
    /// Null for public repositories, and when the connection's repository
    /// list does not include it (yet).
    pub repository_id: Option<i32>,
    /// HTTPS clone URL, when known.
    pub clone_url: Option<String>,
}

/// Where a group's repository is read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepositorySource {
    /// Through a git connection.
    Connection(i32),
    /// Anonymously, from a public repository on `github` or `gitlab`.
    Public(String),
}

/// Input for pointing a group at a repository.
#[derive(Debug, Clone)]
pub struct GroupRepositoryInput {
    pub source: RepositorySource,
    pub repo_owner: String,
    pub repo_name: String,
    pub main_branch: String,
}

/// Anonymous HTTPS clone URL of a public repository.
fn public_clone_url(provider: &str, owner: &str, name: &str) -> String {
    let host = if provider == "gitlab" {
        "gitlab.com"
    } else {
        "github.com"
    };
    format!("https://{host}/{owner}/{name}.git")
}

/// What happened to one inheriting member when the group's repository
/// changed. `error` is set when that member could not be updated; it then
/// still points at its previous repository or branch.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MemberSyncResult {
    pub project_id: i32,
    pub name: String,
    pub slug: String,
    pub error: Option<String>,
}

pub struct GroupRepositoryService {
    db: Arc<DatabaseConnection>,
    projects: Arc<ProjectService>,
    git: Arc<GitProviderManager>,
}

/// Whether `project` already points at the group's repository and branch.
pub fn follows(group: &project_groups::Model, project: &projects::Model) -> bool {
    points_at(
        group,
        project.git_provider_connection_id,
        project.is_public_repo,
        &project.repo_owner,
        &project.repo_name,
        &project.main_branch,
        project.git_url.as_deref(),
    )
}

/// `url` without scheme, `www.`, a trailing `.git` or slash, lowercased, so
/// two spellings of one clone URL compare equal.
fn normalize_clone_url(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    let url = url
        .strip_suffix(".git")
        .unwrap_or(url)
        .trim_end_matches('/');
    let url = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    url.strip_prefix("www.").unwrap_or(url).to_ascii_lowercase()
}

/// A repository a group points at, as far as telling two repositories apart
/// goes. Borrowed from a stored group or from a [`GroupRepository`].
#[derive(Debug, Clone, Copy)]
pub struct GroupRepositoryRef<'a> {
    pub connection_id: Option<i32>,
    /// `github` or `gitlab` for a repository read anonymously.
    pub public_provider: Option<&'a str>,
    pub owner: &'a str,
    pub name: &'a str,
    pub branch: &'a str,
}

impl<'a> From<&'a GroupRepository> for GroupRepositoryRef<'a> {
    fn from(repo: &'a GroupRepository) -> Self {
        Self {
            connection_id: repo.git_provider_connection_id,
            public_provider: repo.public_provider.as_deref(),
            owner: &repo.repo_owner,
            name: &repo.repo_name,
            branch: &repo.main_branch,
        }
    }
}

/// Where a member project, or a service about to become one, reads its code.
#[derive(Debug, Clone, Copy)]
pub struct MemberRepositoryRef<'a> {
    pub connection_id: Option<i32>,
    pub is_public: bool,
    pub owner: &'a str,
    pub name: &'a str,
    pub branch: &'a str,
    pub git_url: Option<&'a str>,
}

/// Whether `member` reads the repository and branch `group` points at.
///
/// The single definition of "follows the group's repository", shared by
/// [`follows`] (existing members) and the instantiation engine (a service
/// about to join), so the two cannot disagree about which repository is the
/// group's. Owner, name and branch alone do not identify a repository: the
/// same `acme/shop` exists on every provider and on every self-hosted
/// instance, so the place it is read from must match too:
///
/// - a connected repository is read through exactly one connection, which
///   must be the group's;
/// - a public one is cloned anonymously from the group's provider, so the
///   member must be public, have no connection, and clone from the URL the
///   group's provider would give (compared spelling-insensitively).
///
/// Owner and name compare case-insensitively (hosts treat `Acme/Shop` and
/// `acme/shop` as one repository); the branch is case-sensitive.
pub fn same_repository(group: &GroupRepositoryRef<'_>, member: &MemberRepositoryRef<'_>) -> bool {
    if !member.owner.eq_ignore_ascii_case(group.owner)
        || !member.name.eq_ignore_ascii_case(group.name)
        || member.branch != group.branch
        || member.connection_id != group.connection_id
    {
        return false;
    }
    match group.public_provider {
        // A public group's members clone anonymously, from the group's URL.
        // Moving a group from one host to another with the same names leaves
        // every member pointing at the old host, so the URL must match too.
        Some(provider) => {
            member.is_public
                && member.git_url.is_some_and(|url| {
                    normalize_clone_url(url)
                        == normalize_clone_url(&public_clone_url(provider, group.owner, group.name))
                })
        }
        None => true,
    }
}

fn points_at(
    group: &project_groups::Model,
    connection_id: Option<i32>,
    is_public_repo: bool,
    repo_owner: &str,
    repo_name: &str,
    branch: &str,
    git_url: Option<&str>,
) -> bool {
    let (Some(owner), Some(name), Some(group_branch)) =
        (&group.repo_owner, &group.repo_name, &group.main_branch)
    else {
        return false;
    };
    same_repository(
        &GroupRepositoryRef {
            connection_id: group.git_provider_connection_id,
            public_provider: group.public_provider.as_deref(),
            owner,
            name,
            branch: group_branch,
        },
        &MemberRepositoryRef {
            connection_id,
            is_public: is_public_repo,
            owner: repo_owner,
            name: repo_name,
            branch,
            git_url,
        },
    )
}

fn clean_field(label: &str, value: &str) -> Result<String, ProjectGroupError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(ProjectGroupError::Validation {
            message: format!("{label} is required"),
        });
    }
    if value.len() > MAX_FIELD_LEN {
        return Err(ProjectGroupError::Validation {
            message: format!("{label} must be at most {MAX_FIELD_LEN} characters"),
        });
    }
    if value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(ProjectGroupError::Validation {
            message: format!("{label} cannot contain spaces or control characters"),
        });
    }
    Ok(value.to_string())
}

/// A public repository's owner or name is interpolated into provider API
/// and clone URLs, so it must be exactly one plain path segment.
fn repo_segment(label: &str, value: &str) -> Result<(), ProjectGroupError> {
    let plain = value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !plain || value.starts_with('.') {
        return Err(ProjectGroupError::Validation {
            message: format!(
                "{label} may only contain letters, digits, '-', '_' and '.', and cannot start with '.'"
            ),
        });
    }
    Ok(())
}

/// Confirms a public repository exists, is public and has the branch.
async fn validate_public(
    provider: &str,
    owner: String,
    name: String,
    branch: String,
) -> Result<GroupRepository, ProjectGroupError> {
    let provider = provider.trim().to_ascii_lowercase();
    if !PUBLIC_PROVIDERS.contains(&provider.as_str()) {
        return Err(ProjectGroupError::Validation {
            message: format!(
                "Public repositories can be read from {}; connect other providers under Git providers",
                PUBLIC_PROVIDERS.join(" or ")
            ),
        });
    }
    repo_segment("Repository owner", &owner)?;
    repo_segment("Repository name", &name)?;

    let unavailable = |reason: String| ProjectGroupError::PublicRepository {
        provider: provider.clone(),
        owner: owner.clone(),
        name: name.clone(),
        reason,
    };
    let client =
        PublicRepoProviderFactory::create(&provider).map_err(|e| unavailable(e.to_string()))?;
    let info = client
        .get_repository(&owner, &name)
        .await
        .map_err(|e| unavailable(public_error_reason(e)))?;
    if info.is_private {
        return Err(unavailable(
            "it is private; connect it through a git provider instead".to_string(),
        ));
    }
    if branch != info.default_branch {
        let exists = ref_exists(client.as_ref(), &owner, &name, &branch)
            .await
            .map_err(|e| unavailable(public_error_reason(e)))?;
        if !exists {
            return Err(ProjectGroupError::BranchNotFound {
                owner: owner.clone(),
                name: name.clone(),
                branch: branch.clone(),
                reason: format!(
                    "it is not a branch, tag or commit of the repository; the default branch is {}",
                    info.default_branch
                ),
            });
        }
    }
    Ok(GroupRepository {
        git_provider_connection_id: None,
        clone_url: Some(public_clone_url(&provider, &owner, &name)),
        public_provider: Some(provider),
        repo_owner: owner,
        repo_name: name,
        main_branch: branch,
        repository_id: None,
    })
}

/// Whether `reference` is a branch, tag or commit of a public repository.
///
/// Templates pin their source to a release tag or a commit as often as to a
/// branch, and a branch-only lookup would reject those and strand the
/// instantiation. This uses the same resolution a deployment does
/// (`resolve_ref`: branch, then tag, then commit), so a ref accepted here is
/// one the deployment can check out.
async fn ref_exists(
    client: &dyn PublicRepoProvider,
    owner: &str,
    name: &str,
    reference: &str,
) -> Result<bool, PublicRepoError> {
    Ok(client.resolve_ref(owner, name, reference).await?.is_some())
}

/// A safe, actionable reason for a failed public-repository lookup. Provider
/// messages can carry upstream API detail, so they are logged, not returned.
fn public_error_reason(error: PublicRepoError) -> String {
    let reason = match &error {
        PublicRepoError::NotFound(_)
        | PublicRepoError::RepositoryNotPublic(_)
        | PublicRepoError::PermissionDenied { .. } => "it does not exist or is not public",
        PublicRepoError::RateLimitExceeded => {
            "the provider's rate limit for anonymous requests was reached; try again later"
        }
        PublicRepoError::BranchNotFound(_) => "the branch does not exist",
        PublicRepoError::ProviderNotSupported(_) => "this provider is not supported",
        PublicRepoError::ResponseTooLarge { .. } => "the provider's response was too large",
        PublicRepoError::ApiError(_) | PublicRepoError::Internal(_) => {
            "the provider could not be reached or returned an error; try again later"
        }
    };
    tracing::warn!(error = %error, "public repository lookup for a project group failed");
    reason.to_string()
}

impl GroupRepositoryService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        projects: Arc<ProjectService>,
        git: Arc<GitProviderManager>,
    ) -> Self {
        Self { db, projects, git }
    }

    async fn find_group(&self, group_id: i32) -> Result<project_groups::Model, ProjectGroupError> {
        project_groups::Entity::find_by_id(group_id)
            .one(self.db.as_ref())
            .await
            .map_err(ProjectGroupError::db(format!(
                "loading project group {group_id}"
            )))?
            .ok_or(ProjectGroupError::NotFound { group_id })
    }

    async fn find_repository(
        &self,
        connection_id: i32,
        owner: &str,
        name: &str,
    ) -> Result<Option<repositories::Model>, ProjectGroupError> {
        repositories::Entity::find()
            .filter(repositories::Column::GitProviderConnectionId.eq(connection_id))
            .filter(repositories::Column::Owner.eq(owner))
            .filter(repositories::Column::Name.eq(name))
            .one(self.db.as_ref())
            .await
            .map_err(ProjectGroupError::db(format!(
                "looking up repository {owner}/{name} on connection {connection_id}"
            )))
    }

    /// The group's repository, or `None` when it has none.
    pub async fn repository_of(
        &self,
        group: &project_groups::Model,
    ) -> Result<Option<GroupRepository>, ProjectGroupError> {
        let (Some(owner), Some(name), Some(branch)) =
            (&group.repo_owner, &group.repo_name, &group.main_branch)
        else {
            return Ok(None);
        };
        let record = match group.git_provider_connection_id {
            Some(connection_id) => self.find_repository(connection_id, owner, name).await?,
            None => None,
        };
        let clone_url = match &group.public_provider {
            Some(provider) => Some(public_clone_url(provider, owner, name)),
            None => record.as_ref().and_then(|r| r.clone_url.clone()),
        };
        Ok(Some(GroupRepository {
            git_provider_connection_id: group.git_provider_connection_id,
            public_provider: group.public_provider.clone(),
            repo_owner: owner.clone(),
            repo_name: name.clone(),
            main_branch: branch.clone(),
            repository_id: record.map(|r| r.id),
            clone_url,
        }))
    }

    /// Points `project` at the group's repository and branch through the
    /// same path as the project's own git settings, keeping its directory.
    async fn write_through(
        &self,
        repo: &GroupRepository,
        project: &projects::Model,
        caller: DeployCaller,
    ) -> Result<(), ProjectGroupError> {
        if project.source_type != SourceType::Git {
            return Err(ProjectGroupError::NotGitSourced {
                project_id: project.id,
                name: project.name.clone(),
                source_type: format!("{:?}", project.source_type).to_lowercase(),
            });
        }
        // `Some(0)` explicitly drops the member's connection: a public
        // repository is cloned anonymously from its URL.
        let (connection, is_public) = match (repo.git_provider_connection_id, &repo.public_provider)
        {
            (Some(connection_id), _) => (connection_id, false),
            (None, Some(_)) => (0, true),
            (None, None) => {
                return Err(ProjectGroupError::GitConnection {
                    connection_id: 0,
                    reason: "the project's repository has no git connection; connect it again"
                        .to_string(),
                })
            }
        };
        self.projects
            .update_git_settings(
                project.id,
                Some(connection),
                repo.main_branch.clone(),
                repo.repo_owner.clone(),
                repo.repo_name.clone(),
                None,
                project.directory.clone(),
                None,
                repo.clone_url.clone(),
                Some(is_public),
                None,
                caller,
            )
            .await
            .map_err(|source| ProjectGroupError::ServiceGit {
                project_id: project.id,
                name: project.name.clone(),
                source,
            })?;
        Ok(())
    }

    /// Validates the source, repository and branch before anything is
    /// written, so a typo fails once instead of once per member.
    async fn validate(
        &self,
        input: GroupRepositoryInput,
    ) -> Result<GroupRepository, ProjectGroupError> {
        let owner = clean_field("Repository owner", &input.repo_owner)?;
        let name = clean_field("Repository name", &input.repo_name)?;
        let branch = clean_field("Branch", &input.main_branch)?;
        match input.source {
            RepositorySource::Connection(connection_id) => {
                self.validate_connected(connection_id, owner, name, branch)
                    .await
            }
            RepositorySource::Public(provider) => {
                validate_public(&provider, owner, name, branch).await
            }
        }
    }

    /// Git connections are installation-wide: any caller allowed to change a
    /// project's git settings may use any active connection, exactly as the
    /// per-project path (`ProjectService::update_git_settings`) does. The
    /// repository must also be in that connection's synced list, so owner and
    /// name always match a record the provider returned.
    async fn validate_connected(
        &self,
        connection_id: i32,
        owner: String,
        name: String,
        branch: String,
    ) -> Result<GroupRepository, ProjectGroupError> {
        let connection = git_provider_connections::Entity::find_by_id(connection_id)
            .one(self.db.as_ref())
            .await
            .map_err(ProjectGroupError::db(format!(
                "loading git connection {connection_id}"
            )))?
            .ok_or_else(|| ProjectGroupError::GitConnection {
                connection_id,
                reason: "it does not exist".to_string(),
            })?;
        if !connection.is_active {
            return Err(ProjectGroupError::GitConnection {
                connection_id,
                reason: "it is not active; reconnect it under Git providers".to_string(),
            });
        }
        let record = self
            .find_repository(connection_id, &owner, &name)
            .await?
            .ok_or_else(|| ProjectGroupError::RepositoryUnavailable {
                connection_id,
                owner: owner.clone(),
                name: name.clone(),
            })?;
        self.git
            .get_branch_latest_commit(connection_id, &owner, &name, &branch)
            .await
            .map_err(|e| {
                // Provider errors can carry upstream API detail: log them,
                // return a safe reason.
                tracing::warn!(
                    connection_id,
                    owner = %owner,
                    name = %name,
                    branch = %branch,
                    error = %e,
                    "verifying a project group's branch failed"
                );
                ProjectGroupError::BranchNotFound {
                    owner: owner.clone(),
                    name: name.clone(),
                    branch: branch.clone(),
                    reason: "it does not exist, or the connection cannot read it".to_string(),
                }
            })?;
        Ok(GroupRepository {
            git_provider_connection_id: Some(connection_id),
            public_provider: None,
            repo_owner: owner,
            repo_name: name,
            main_branch: branch,
            repository_id: Some(record.id),
            clone_url: record.clone_url,
        })
    }

    /// Sets the group's repository and writes it through to every member
    /// that follows it. Members are updated one by one; one failing does not
    /// stop the rest, and each outcome is reported.
    ///
    /// `authorized_follower_ids` is the set of followers the caller was
    /// checked to have write access on. The provider round trip below can
    /// take a while, and another member may start following during it; the
    /// followers are therefore read again under lock, and when one of them
    /// is not in the set nothing is changed and
    /// [`ProjectGroupError::RepositoryFollowersChanged`] is returned.
    /// Followers that left in the meantime are simply skipped.
    pub async fn set_repository(
        &self,
        group_id: i32,
        input: GroupRepositoryInput,
        authorized_follower_ids: &HashSet<i32>,
        caller: DeployCaller,
    ) -> Result<(project_groups::Model, Vec<MemberSyncResult>), ProjectGroupError> {
        // A missing group fails before the provider round trip.
        self.find_group(group_id).await?;
        let repo = self.validate(input).await?;
        self.apply_repository(group_id, &repo, authorized_follower_ids, caller)
            .await
    }

    /// Stores `repo` on the group and writes it through, once the followers
    /// have been checked under lock (see [`Self::set_repository`]).
    ///
    /// The group row and the follower check share one transaction, so a
    /// refused change leaves the group as it was. The write-through runs
    /// after it commits (it goes through other connections and the git
    /// provider) but only for the followers that transaction saw, all of
    /// them authorized; a project that starts following later is written by
    /// its own [`Self::set_inheritance`], which reads the group after this
    /// commit.
    async fn apply_repository(
        &self,
        group_id: i32,
        repo: &GroupRepository,
        authorized_follower_ids: &HashSet<i32>,
        caller: DeployCaller,
    ) -> Result<(project_groups::Model, Vec<MemberSyncResult>), ProjectGroupError> {
        let txn = self
            .db
            .begin()
            .await
            .map_err(ProjectGroupError::db(format!(
                "starting transaction to set the repository of project group {group_id}"
            )))?;
        let result = async {
            let locked = ProjectGroupService::lock_repository_followers(
                &txn,
                group_id,
                authorized_follower_ids,
            )
            .await?;
            let mut active: project_groups::ActiveModel = locked.group.into();
            active.git_provider_connection_id = Set(repo.git_provider_connection_id);
            active.public_provider = Set(repo.public_provider.clone());
            active.repo_owner = Set(Some(repo.repo_owner.clone()));
            active.repo_name = Set(Some(repo.repo_name.clone()));
            active.main_branch = Set(Some(repo.main_branch.clone()));
            active.updated_at = Set(Utc::now());
            let group = active
                .update(&txn)
                .await
                .map_err(ProjectGroupError::db(format!(
                    "saving the repository of project group {group_id}"
                )))?;
            Ok::<_, ProjectGroupError>((group, locked.followers))
        }
        .await;
        let (group, followers) = ProjectGroupService::end_transaction(
            txn,
            result,
            format!("repository change of project group {group_id}"),
        )
        .await?;

        let mut results = Vec::with_capacity(followers.len());
        for project in followers {
            let error = if follows(&group, &project) {
                None
            } else {
                self.write_through(repo, &project, caller)
                    .await
                    .err()
                    .map(|e| e.to_string())
            };
            results.push(MemberSyncResult {
                project_id: project.id,
                name: project.name,
                slug: project.slug,
                error,
            });
        }
        Ok((group, results))
    }

    /// Removes the group's repository. Members stop following it and keep
    /// the repository and branch they have now; nothing is redeployed.
    ///
    /// Detaching changes every follower, so the caller must have been
    /// checked on exactly `authorized_follower_ids`: the followers are read
    /// again under lock, and one that is not in the set fails the call with
    /// [`ProjectGroupError::RepositoryFollowersChanged`], changing nothing.
    ///
    /// Both writes (detach members, clear group row) happen inside a single
    /// transaction so a partial failure cannot leave some members still marked
    /// as inheriting a repository that the group no longer has.
    pub async fn clear_repository(
        &self,
        group_id: i32,
        authorized_follower_ids: &HashSet<i32>,
    ) -> Result<project_groups::Model, ProjectGroupError> {
        let txn = self
            .db
            .begin()
            .await
            .map_err(ProjectGroupError::db(format!(
                "starting transaction for clearing repository of project group {group_id}"
            )))?;
        let result = async {
            let locked = ProjectGroupService::lock_repository_followers(
                &txn,
                group_id,
                authorized_follower_ids,
            )
            .await?;
            project_group_members::Entity::update_many()
                .col_expr(
                    project_group_members::Column::InheritsRepository,
                    sea_orm::sea_query::Expr::value(false),
                )
                .filter(project_group_members::Column::GroupId.eq(group_id))
                .exec(&txn)
                .await
                .map_err(ProjectGroupError::db(format!(
                    "detaching members of project group {group_id} from its repository"
                )))?;
            let mut active: project_groups::ActiveModel = locked.group.into();
            active.git_provider_connection_id = Set(None);
            active.public_provider = Set(None);
            active.repo_owner = Set(None);
            active.repo_name = Set(None);
            active.main_branch = Set(None);
            active.updated_at = Set(Utc::now());
            active
                .update(&txn)
                .await
                .map_err(ProjectGroupError::db(format!(
                    "clearing the repository of project group {group_id}"
                )))
        }
        .await;
        ProjectGroupService::end_transaction(
            txn,
            result,
            format!("clearing the repository of project group {group_id}"),
        )
        .await
    }

    /// Makes a member follow the group's repository (pointing it there now)
    /// or stop following it (keeping its current settings). The flag only
    /// changes once the member's git settings were updated successfully.
    ///
    /// Only `project_id` is rewritten, so the caller needs write access to
    /// that project alone. The flag is set under the lock order of
    /// [`ProjectGroupService`], re-reading the membership and the group: a
    /// project that was moved to another group meanwhile is not marked as
    /// following this one, and a repository that changed after the project
    /// was pointed at it fails with
    /// [`ProjectGroupError::RepositoryChanged`] instead of leaving the
    /// project following a repository the group no longer has.
    pub async fn set_inheritance(
        &self,
        group_id: i32,
        project_id: i32,
        inherit: bool,
        caller: DeployCaller,
    ) -> Result<(), ProjectGroupError> {
        let group = self.find_group(group_id).await?;
        let membership = project_group_members::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
            .map_err(ProjectGroupError::db(format!(
                "loading group membership of project {project_id}"
            )))?
            .filter(|m| m.group_id == group_id)
            .ok_or(ProjectGroupError::NotAMember {
                group_id,
                project_id,
            })?;
        if membership.inherits_repository == inherit {
            return Ok(());
        }

        let mut pointed_at: Option<GroupRepository> = None;
        if inherit {
            let repo = self
                .repository_of(&group)
                .await?
                .ok_or(ProjectGroupError::RepositoryNotSet { group_id })?;
            let project = projects::Entity::find_by_id(project_id)
                .filter(projects::Column::IsDeleted.eq(false))
                .one(self.db.as_ref())
                .await
                .map_err(ProjectGroupError::db(format!(
                    "loading project {project_id}"
                )))?
                .ok_or(ProjectGroupError::ProjectNotFound { project_id })?;
            if !follows(&group, &project) {
                self.write_through(&repo, &project, caller).await?;
            }
            pointed_at = Some(repo);
        }

        let txn = self
            .db
            .begin()
            .await
            .map_err(ProjectGroupError::db(format!(
                "starting transaction to change repository inheritance of project {project_id}"
            )))?;
        let result = async {
            // Lock order: the project, the group row, then the membership.
            let locked = ProjectGroupService::lock_projects(&txn, &[project_id]).await?;
            if inherit && locked.get(&project_id) != Some(&false) {
                return Err(ProjectGroupError::ProjectNotFound { project_id });
            }
            let current = project_groups::Entity::find_by_id(group_id)
                .lock(LockType::NoKeyUpdate)
                .one(&txn)
                .await
                .map_err(ProjectGroupError::db(format!(
                    "loading project group {group_id}"
                )))?
                .ok_or(ProjectGroupError::NotFound { group_id })?;
            let membership = project_group_members::Entity::find_by_id(project_id)
                .filter(project_group_members::Column::GroupId.eq(group_id))
                .lock_exclusive()
                .one(&txn)
                .await
                .map_err(ProjectGroupError::db(format!(
                    "locking group membership of project {project_id}"
                )))?
                .ok_or(ProjectGroupError::NotAMember {
                    group_id,
                    project_id,
                })?;
            if let Some(repo) = &pointed_at {
                if !is_repository_of(&current, repo) {
                    return Err(ProjectGroupError::RepositoryChanged {
                        group_id,
                        project_id,
                    });
                }
            }
            let mut active: project_group_members::ActiveModel = membership.into();
            active.inherits_repository = Set(inherit);
            active
                .update(&txn)
                .await
                .map_err(ProjectGroupError::db(format!(
                    "updating repository inheritance of project {project_id}"
                )))?;
            Ok::<_, ProjectGroupError>(())
        }
        .await;
        ProjectGroupService::end_transaction(
            txn,
            result,
            format!("repository inheritance change of project {project_id}"),
        )
        .await
    }
}

/// Whether `group` still stores exactly `repo` (as returned by
/// [`GroupRepositoryService::repository_of`]).
fn is_repository_of(group: &project_groups::Model, repo: &GroupRepository) -> bool {
    group.git_provider_connection_id == repo.git_provider_connection_id
        && group.public_provider == repo.public_provider
        && group.repo_owner.as_deref() == Some(repo.repo_owner.as_str())
        && group.repo_name.as_deref() == Some(repo.repo_name.as_str())
        && group.main_branch.as_deref() == Some(repo.main_branch.as_str())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn group(connection: Option<i32>, repo: Option<(&str, &str, &str)>) -> project_groups::Model {
        let now = Utc::now();
        project_groups::Model {
            id: 1,
            name: "Acme Shop".into(),
            slug: "acme-shop".into(),
            description: None,
            git_provider_connection_id: connection,
            repo_owner: repo.map(|r| r.0.to_string()),
            repo_name: repo.map(|r| r.1.to_string()),
            main_branch: repo.map(|r| r.2.to_string()),
            public_provider: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn follows_requires_same_connection_repo_and_branch() {
        let g = group(Some(7), Some(("acme", "shop", "main")));
        assert!(points_at(&g, Some(7), false, "acme", "shop", "main", None));
        assert!(!points_at(
            &g,
            Some(7),
            false,
            "acme",
            "shop",
            "develop",
            None
        ));
        assert!(!points_at(
            &g,
            Some(7),
            false,
            "acme",
            "other",
            "main",
            None
        ));
        assert!(!points_at(&g, Some(8), false, "acme", "shop", "main", None));
    }

    #[test]
    fn a_public_group_is_followed_only_by_anonymous_clones() {
        let mut g = group(None, Some(("acme", "shop", "main")));
        g.public_provider = Some("github".into());
        let url = Some("https://github.com/acme/shop.git");
        assert!(points_at(&g, None, true, "acme", "shop", "main", url));
        // Same repo but still set up to clone through a (deleted) connection.
        assert!(!points_at(&g, None, false, "acme", "shop", "main", url));
        assert!(!points_at(&g, Some(7), true, "acme", "shop", "main", url));
    }

    #[test]
    fn moving_a_public_group_to_another_provider_unsynchronizes_its_members() {
        // Same owner, name and branch; only the host differs.
        let mut g = group(None, Some(("acme", "shop", "main")));
        g.public_provider = Some("gitlab".into());
        let old = Some("https://github.com/acme/shop.git");
        let new = Some("https://gitlab.com/acme/shop.git");
        assert!(
            !points_at(&g, None, true, "acme", "shop", "main", old),
            "a member still cloning from the old host must be written through"
        );
        assert!(points_at(&g, None, true, "acme", "shop", "main", new));

        // The other direction.
        g.public_provider = Some("github".into());
        assert!(!points_at(&g, None, true, "acme", "shop", "main", new));
        assert!(points_at(&g, None, true, "acme", "shop", "main", old));
    }

    #[test]
    fn a_public_member_without_a_clone_url_is_written_through() {
        let mut g = group(None, Some(("acme", "shop", "main")));
        g.public_provider = Some("github".into());
        assert!(!points_at(&g, None, true, "acme", "shop", "main", None));
        assert!(!points_at(&g, None, true, "acme", "shop", "main", Some("")));
    }

    #[test]
    fn clone_urls_compare_across_spellings() {
        let mut g = group(None, Some(("acme", "shop", "main")));
        g.public_provider = Some("github".into());
        for spelling in [
            "https://github.com/acme/shop",
            "https://github.com/acme/shop.git",
            "https://github.com/acme/shop/",
            "http://www.github.com/acme/shop.git",
            "https://GitHub.com/acme/shop.git",
        ] {
            assert!(
                points_at(&g, None, true, "acme", "shop", "main", Some(spelling)),
                "{spelling}"
            );
        }
        // A look-alike host or a different path is a different repository.
        for other in [
            "https://github.com.evil.test/acme/shop.git",
            "https://github.com/acme/shop-fork.git",
            "git@github.com:acme/shop.git",
        ] {
            assert!(
                !points_at(&g, None, true, "acme", "shop", "main", Some(other)),
                "{other}"
            );
        }
    }

    fn public_group_ref(provider: &'static str) -> GroupRepositoryRef<'static> {
        GroupRepositoryRef {
            connection_id: None,
            public_provider: Some(provider),
            owner: "acme",
            name: "shop",
            branch: "main",
        }
    }

    fn public_member(url: &str) -> MemberRepositoryRef<'_> {
        MemberRepositoryRef {
            connection_id: None,
            is_public: true,
            owner: "acme",
            name: "shop",
            branch: "main",
            git_url: Some(url),
        }
    }

    #[test]
    fn same_names_on_another_provider_are_not_the_same_repository() {
        let github = "https://github.com/acme/shop.git";
        let gitlab = "https://gitlab.com/acme/shop.git";
        assert!(same_repository(
            &public_group_ref("github"),
            &public_member(github)
        ));
        assert!(same_repository(
            &public_group_ref("gitlab"),
            &public_member(gitlab)
        ));
        // Both directions: identical owner, name and branch, other host.
        assert!(!same_repository(
            &public_group_ref("github"),
            &public_member(gitlab)
        ));
        assert!(!same_repository(
            &public_group_ref("gitlab"),
            &public_member(github)
        ));
    }

    #[test]
    fn a_repository_is_the_same_across_url_spellings_and_owner_case() {
        let group = public_group_ref("github");
        for url in [
            "https://github.com/acme/shop",
            "https://github.com/acme/shop.git",
            "http://www.github.com/acme/shop.git/",
            "https://GitHub.com/Acme/Shop.git",
        ] {
            let mut member = public_member(url);
            // The owner and name a template parses out of such a URL keep
            // the URL's spelling.
            if url.contains("Acme") {
                member.owner = "Acme";
                member.name = "Shop";
            }
            assert!(same_repository(&group, &member), "{url}");
        }
        // A branch is not case-folded.
        let mut other_branch = public_member("https://github.com/acme/shop.git");
        other_branch.branch = "Main";
        assert!(!same_repository(&group, &other_branch));
    }

    #[test]
    fn a_connected_repository_is_only_followed_through_the_same_connection() {
        let group = GroupRepositoryRef {
            connection_id: Some(7),
            public_provider: None,
            owner: "acme",
            name: "shop",
            branch: "main",
        };
        let member = |connection_id: Option<i32>, is_public: bool, url: Option<&'static str>| {
            MemberRepositoryRef {
                connection_id,
                is_public,
                owner: "acme",
                name: "shop",
                branch: "main",
                git_url: url,
            }
        };
        assert!(same_repository(&group, &member(Some(7), false, None)));
        // Another connection (a second self-hosted instance with the same
        // owner and name) and an anonymous clone are other repositories.
        assert!(!same_repository(&group, &member(Some(8), false, None)));
        assert!(!same_repository(
            &group,
            &member(None, true, Some("https://github.com/acme/shop.git"))
        ));
    }

    #[test]
    fn public_repository_names_must_be_one_plain_segment() {
        for good in ["vercel", "next.js", "my_repo-2"] {
            assert!(repo_segment("Repository name", good).is_ok(), "{good}");
        }
        for bad in ["a/b", "..", ".hidden", "a?b", "a#b", "a%2fb", "ü"] {
            assert!(repo_segment("Repository name", bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn public_clone_urls_point_at_the_provider_host() {
        assert_eq!(
            public_clone_url("github", "acme", "shop"),
            "https://github.com/acme/shop.git"
        );
        assert_eq!(
            public_clone_url("gitlab", "acme", "shop"),
            "https://gitlab.com/acme/shop.git"
        );
    }

    #[test]
    fn nothing_follows_a_group_without_a_repository() {
        let g = group(None, None);
        assert!(!points_at(&g, None, false, "", "", "", None));
    }

    #[test]
    fn clean_field_trims_and_rejects_unsafe_values() {
        assert_eq!(clean_field("Branch", "  main ").unwrap(), "main");
        assert_eq!(
            clean_field("Repository owner", "group/subgroup").unwrap(),
            "group/subgroup"
        );
        for bad in ["", "   ", "feature branch", "ma\nin", "a\u{7}b"] {
            assert!(
                clean_field("Branch", bad).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
        assert!(clean_field("Branch", &"b".repeat(MAX_FIELD_LEN + 1)).is_err());
    }

    use temps_git::services::git_provider::FileContent;
    use temps_git::services::public_repo::{PublicBranch, PublicRepoInfo};

    /// A public provider whose branches, and the tags and commits its lookups
    /// resolve, are fixed, counting the tag lookups made.
    struct FakeProvider {
        branches: Vec<String>,
        /// Tag names and commit ids `get_tag_commit` / `get_commit_sha` resolve.
        refs: Vec<String>,
        tag_error: Option<fn() -> PublicRepoError>,
        branch_error: Option<fn() -> PublicRepoError>,
        tag_calls: std::sync::atomic::AtomicUsize,
    }

    impl FakeProvider {
        fn new(branches: &[&'static str], refs: &[&'static str]) -> Self {
            Self {
                branches: branches.iter().map(|b| (*b).to_string()).collect(),
                refs: refs.iter().map(|t| (*t).to_string()).collect(),
                tag_error: None,
                branch_error: None,
                tag_calls: Default::default(),
            }
        }
    }

    #[async_trait::async_trait]
    impl PublicRepoProvider for FakeProvider {
        fn provider_name(&self) -> &'static str {
            "fake"
        }

        async fn get_repository(
            &self,
            _owner: &str,
            _repo: &str,
        ) -> Result<PublicRepoInfo, PublicRepoError> {
            Err(PublicRepoError::Internal("not used".into()))
        }

        async fn list_branches(
            &self,
            _owner: &str,
            _repo: &str,
        ) -> Result<Vec<PublicBranch>, PublicRepoError> {
            Ok(Vec::new())
        }

        async fn get_branch(
            &self,
            _owner: &str,
            _repo: &str,
            branch: &str,
        ) -> Result<Option<PublicBranch>, PublicRepoError> {
            if let Some(error) = self.branch_error {
                return Err(error());
            }
            Ok(self
                .branches
                .iter()
                .any(|b| *b == branch)
                .then(|| PublicBranch {
                    name: branch.to_string(),
                    commit_sha: "0".repeat(40),
                    protected: false,
                }))
        }

        async fn get_tag_commit(
            &self,
            _owner: &str,
            _repo: &str,
            tag: &str,
        ) -> Result<Option<String>, PublicRepoError> {
            self.tag_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if let Some(error) = self.tag_error {
                return Err(error());
            }
            Ok(self
                .refs
                .iter()
                .any(|t| t == tag && !tag.bytes().all(|b| b.is_ascii_hexdigit()))
                .then(|| "1".repeat(40)))
        }

        async fn get_commit_sha(
            &self,
            _owner: &str,
            _repo: &str,
            sha: &str,
        ) -> Result<Option<String>, PublicRepoError> {
            Ok(self.refs.iter().find(|r| r.starts_with(sha)).cloned())
        }

        async fn get_file_tree(
            &self,
            _owner: &str,
            _repo: &str,
            _reference: &str,
        ) -> Result<Vec<String>, PublicRepoError> {
            Err(PublicRepoError::Internal("not used".into()))
        }

        async fn get_file_content(
            &self,
            _owner: &str,
            _repo: &str,
            _path: &str,
            _reference: &str,
        ) -> Result<FileContent, PublicRepoError> {
            Err(PublicRepoError::Internal("not used".into()))
        }
    }

    #[tokio::test]
    async fn a_branch_is_confirmed_without_looking_up_tags() {
        let provider = FakeProvider::new(&["main", "develop"], &[]);
        assert!(ref_exists(&provider, "acme", "shop", "develop")
            .await
            .expect("lookup"));
        assert_eq!(
            provider.tag_calls.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }

    #[tokio::test]
    async fn a_tag_or_commit_pinned_by_a_template_is_accepted() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let provider = FakeProvider::new(&["main"], &["v1.2.3", sha]);
        for reference in ["v1.2.3", sha, &sha[..8]] {
            assert!(
                ref_exists(&provider, "acme", "shop", reference)
                    .await
                    .expect("lookup"),
                "{reference} must be accepted"
            );
        }
    }

    #[tokio::test]
    async fn a_ref_that_is_nothing_is_rejected() {
        let provider = FakeProvider::new(&["main"], &["v1.2.3"]);
        for reference in ["v9.9.9", "0123456789abcdef"] {
            assert!(
                !ref_exists(&provider, "acme", "shop", reference)
                    .await
                    .expect("lookup"),
                "{reference} must be rejected"
            );
        }
    }

    #[tokio::test]
    async fn provider_failures_are_not_reported_as_a_missing_ref() {
        let mut provider = FakeProvider::new(&["main"], &[]);
        provider.tag_error = Some(|| PublicRepoError::RateLimitExceeded);
        assert!(matches!(
            ref_exists(&provider, "acme", "shop", "v1.2.3").await,
            Err(PublicRepoError::RateLimitExceeded)
        ));

        let mut provider = FakeProvider::new(&["main"], &[]);
        provider.branch_error = Some(|| PublicRepoError::ApiError("HTTP 500".into()));
        assert!(matches!(
            ref_exists(&provider, "acme", "shop", "main").await,
            Err(PublicRepoError::ApiError(_))
        ));
    }

    // -----------------------------------------------------------------
    // Database-backed behaviour. These boot a TimescaleDB container and
    // skip (not fail) when no container runtime is available.
    // -----------------------------------------------------------------

    use crate::service::{CreateProjectGroupRequest, ProjectGroupService};
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
    use temps_database::test_utils::TestDatabase;

    struct NoQueue;

    #[async_trait::async_trait]
    impl temps_core::JobQueue for NoQueue {
        async fn send(&self, _job: temps_core::Job) -> Result<(), temps_core::QueueError> {
            Ok(())
        }
        fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
            unimplemented!("repository tests never subscribe")
        }
    }

    pub(crate) async fn repository_service(
        db: Arc<DatabaseConnection>,
    ) -> Option<GroupRepositoryService> {
        let (projects, git) = project_service(db.clone()).await?;
        Some(GroupRepositoryService::new(db, projects, git))
    }

    /// A `ProjectService` over `db` whose collaborators are never exercised
    /// by tests that only read or delete project rows.
    pub(crate) async fn project_service(
        db: Arc<DatabaseConnection>,
    ) -> Option<(Arc<ProjectService>, Arc<GitProviderManager>)> {
        let server_config = Arc::new(
            temps_config::ServerConfig::new(
                "127.0.0.1:3000".to_string(),
                "postgresql://test".to_string(),
                None,
                None,
            )
            .ok()?,
        );
        let config = Arc::new(temps_config::ConfigService::new(server_config, db.clone()));
        let encryption = Arc::new(
            temps_core::EncryptionService::new(&"0".repeat(64)).expect("test encryption key"),
        );
        let queue: Arc<dyn temps_core::JobQueue> = Arc::new(NoQueue);
        // Never used by these tests: clearing and un-following touch no containers.
        let docker = Arc::new(bollard::Docker::connect_with_local_defaults().ok()?);
        let external = Arc::new(temps_providers::ExternalServiceManager::new(
            db.clone(),
            encryption.clone(),
            docker,
            Arc::new(temps_providers::DnsRegistry::new(db.clone())),
        ));
        let git = Arc::new(GitProviderManager::new(
            db.clone(),
            encryption.clone(),
            queue.clone(),
            config.clone(),
        ));
        let environments = Arc::new(temps_environments::EnvironmentService::new(
            db.clone(),
            config.clone(),
        ));
        let projects = Arc::new(ProjectService::new(
            db.clone(),
            queue,
            config,
            external,
            git.clone(),
            environments,
            encryption,
        ));
        Some((projects, git))
    }

    pub(crate) async fn insert_project(conn: &impl ConnectionTrait, id: i32, name: &str) {
        let stmt = Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            INSERT INTO projects (
                id, name, repo_name, repo_owner, directory, main_branch,
                preset, preset_config, deployment_config, created_at, updated_at,
                slug, is_deleted, deleted_at, last_deployment, is_public_repo,
                git_url, git_provider_connection_id, attack_mode,
                enable_preview_environments, source_type
            )
            VALUES (
                $1, $2, 'shop', 'acme', $3, 'main',
                'nextjs', NULL, NULL, NOW(), NOW(),
                $4, FALSE, NULL, NULL, TRUE,
                NULL, NULL, FALSE, FALSE, 'git'
            )
            "#,
            [
                id.into(),
                name.into(),
                format!("apps/{name}").into(),
                format!("{name}-{id}").into(),
            ],
        );
        conn.execute(stmt).await.expect("insert project");
    }

    /// A group on the public `acme/shop` repository whose two members both
    /// follow it, as if the repository had been set earlier.
    async fn followed_group(
        conn: &Arc<DatabaseConnection>,
        name: &str,
        ids: [i32; 2],
    ) -> project_groups::Model {
        for (id, service) in ids.iter().zip(["web", "api"]) {
            insert_project(conn.as_ref(), *id, service).await;
        }
        let group = ProjectGroupService::new(conn.clone())
            .create_group(CreateProjectGroupRequest {
                name: name.into(),
                slug: None,
                description: None,
                project_ids: ids.to_vec(),
            })
            .await
            .expect("create group")
            .group;
        let mut active: project_groups::ActiveModel = group.into();
        active.public_provider = Set(Some("github".into()));
        active.repo_owner = Set(Some("acme".into()));
        active.repo_name = Set(Some("shop".into()));
        active.main_branch = Set(Some("main".into()));
        let group = active.update(conn.as_ref()).await.expect("set repository");
        project_group_members::Entity::update_many()
            .col_expr(
                project_group_members::Column::InheritsRepository,
                sea_orm::sea_query::Expr::value(true),
            )
            .filter(project_group_members::Column::GroupId.eq(group.id))
            .exec(conn.as_ref())
            .await
            .expect("follow repository");
        group
    }

    async fn following(conn: &DatabaseConnection, group_id: i32) -> Vec<i32> {
        let mut ids: Vec<i32> = project_group_members::Entity::find()
            .filter(project_group_members::Column::GroupId.eq(group_id))
            .filter(project_group_members::Column::InheritsRepository.eq(true))
            .all(conn)
            .await
            .expect("load members")
            .into_iter()
            .map(|member| member.project_id)
            .collect();
        ids.sort();
        ids
    }

    #[tokio::test]
    async fn clearing_the_repository_detaches_every_member_and_keeps_their_settings() {
        let Ok(db) = TestDatabase::with_migrations().await else {
            eprintln!("Docker/TimescaleDB unavailable, skipping repository test");
            return;
        };
        let conn = db.connection_arc();
        let Some(repos) = repository_service(conn.clone()).await else {
            return;
        };
        let group = followed_group(&conn, "Mono", [9301, 9302]).await;
        assert_eq!(following(&conn, group.id).await, vec![9301, 9302]);

        let followers: HashSet<i32> = [9301, 9302].into_iter().collect();
        let cleared = repos
            .clear_repository(group.id, &followers)
            .await
            .expect("clear");
        assert!(cleared.repo_owner.is_none() && cleared.repo_name.is_none());
        assert!(cleared.main_branch.is_none() && cleared.public_provider.is_none());
        assert!(repos.repository_of(&cleared).await.unwrap().is_none());
        assert!(
            following(&conn, group.id).await.is_empty(),
            "no member may follow a repository the group no longer has"
        );
        // Members keep the repository and directory they had.
        let web = projects::Entity::find_by_id(9301)
            .one(conn.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (web.repo_owner.as_str(), web.repo_name.as_str()),
            ("acme", "shop")
        );
        assert_eq!(web.directory, "apps/web");

        assert!(matches!(
            repos.clear_repository(987_654, &HashSet::new()).await,
            Err(ProjectGroupError::NotFound { .. })
        ));
    }

    #[tokio::test]
    async fn inheritance_changes_only_for_members_and_needs_a_repository_to_follow() {
        let Ok(db) = TestDatabase::with_migrations().await else {
            eprintln!("Docker/TimescaleDB unavailable, skipping repository test");
            return;
        };
        let conn = db.connection_arc();
        let Some(repos) = repository_service(conn.clone()).await else {
            return;
        };
        let group = followed_group(&conn, "Mono", [9311, 9312]).await;
        let caller = DeployCaller::from_instance_admin(true);

        // Un-following keeps the service's own git settings untouched.
        repos
            .set_inheritance(group.id, 9311, false, caller)
            .await
            .expect("stop following");
        assert_eq!(following(&conn, group.id).await, vec![9312]);
        // Asking again for what is already true is a no-op, not an error.
        repos
            .set_inheritance(group.id, 9311, false, caller)
            .await
            .expect("idempotent");

        // A service outside the group cannot be switched through it.
        insert_project(conn.as_ref(), 9313, "worker").await;
        assert!(matches!(
            repos.set_inheritance(group.id, 9313, true, caller).await,
            Err(ProjectGroupError::NotAMember { .. })
        ));

        // With the repository gone there is nothing to follow.
        let followers: HashSet<i32> = [9312].into_iter().collect();
        repos
            .clear_repository(group.id, &followers)
            .await
            .expect("clear");
        assert!(matches!(
            repos.set_inheritance(group.id, 9311, true, caller).await,
            Err(ProjectGroupError::RepositoryNotSet { .. })
        ));
        assert!(following(&conn, group.id).await.is_empty());
    }
    // -----------------------------------------------------------------
    // Followers that appear after authorization. Each test holds an
    // uncommitted transaction ("B") that did what a concurrent request
    // would, starts the call under test ("A") while it is open, and checks
    // that A waited for it and then acted on what B committed.
    // -----------------------------------------------------------------

    use std::time::Duration;

    /// A group on `acme/shop@develop` with no connection or provider (so a
    /// member already pointing at `acme/shop@main` is written through only
    /// when the group moves to another branch) holding `members`; only
    /// `followers` follow it.
    async fn group_with_followers(
        conn: &Arc<DatabaseConnection>,
        name: &str,
        members: &[i32],
        followers: &[i32],
    ) -> project_groups::Model {
        for id in members {
            insert_project(conn.as_ref(), *id, &format!("svc{id}")).await;
        }
        let group = ProjectGroupService::new(conn.clone())
            .create_group(CreateProjectGroupRequest {
                name: name.into(),
                slug: None,
                description: None,
                project_ids: members.to_vec(),
            })
            .await
            .expect("create group")
            .group;
        let mut active: project_groups::ActiveModel = group.into();
        active.repo_owner = Set(Some("acme".into()));
        active.repo_name = Set(Some("shop".into()));
        active.main_branch = Set(Some("develop".into()));
        let group = active.update(conn.as_ref()).await.expect("set repository");
        project_group_members::Entity::update_many()
            .col_expr(
                project_group_members::Column::InheritsRepository,
                sea_orm::sea_query::Expr::value(true),
            )
            .filter(project_group_members::Column::GroupId.eq(group.id))
            .filter(project_group_members::Column::ProjectId.is_in(followers.iter().copied()))
            .exec(conn.as_ref())
            .await
            .expect("follow repository");
        group
    }

    /// What the group moves to: the branch every member already points at,
    /// so following members are in sync and nothing is written through.
    fn main_branch_repository() -> GroupRepository {
        GroupRepository {
            git_provider_connection_id: None,
            public_provider: None,
            repo_owner: "acme".into(),
            repo_name: "shop".into(),
            main_branch: "main".into(),
            repository_id: None,
            clone_url: None,
        }
    }

    async fn set_following(conn: &impl ConnectionTrait, project_id: i32, follows: bool) {
        conn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE project_group_members SET inherits_repository = $1 WHERE project_id = $2",
            [follows.into(), project_id.into()],
        ))
        .await
        .expect("change inheritance");
    }

    async fn branch_of(conn: &DatabaseConnection, group_id: i32) -> Option<String> {
        project_groups::Entity::find_by_id(group_id)
            .one(conn)
            .await
            .expect("load group")
            .expect("group exists")
            .main_branch
    }

    /// Runs `fut` on its own task and asserts it is still waiting after a
    /// pause, i.e. blocked on a lock the open transaction holds.
    async fn blocked<T, F>(what: &str, fut: F) -> tokio::task::JoinHandle<T>
    where
        T: Send + 'static,
        F: std::future::Future<Output = T> + Send + 'static,
    {
        let handle = tokio::spawn(fut);
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            !handle.is_finished(),
            "{what} must wait for the concurrent transaction instead of reading around it"
        );
        handle
    }

    async fn done<T>(handle: tokio::task::JoinHandle<T>) -> T {
        match tokio::time::timeout(Duration::from_secs(20), handle).await {
            Ok(Ok(value)) => value,
            Ok(Err(e)) => panic!("task failed: {e}"),
            Err(_) => panic!("task still blocked 20s after the other transaction committed"),
        }
    }

    #[tokio::test]
    async fn a_follower_that_appears_after_authorization_is_rejected_and_nothing_changes() {
        use sea_orm::TransactionTrait;
        let Ok(db) = TestDatabase::with_migrations().await else {
            eprintln!("Docker/TimescaleDB unavailable, skipping repository test");
            return;
        };
        let conn = db.connection_arc();
        let Some(repos) = repository_service(conn.clone()).await else {
            return;
        };
        let repos = Arc::new(repos);
        // 9401 follows and was authorized; 9402 is a member that is not
        // following when the caller is checked.
        let group = group_with_followers(&conn, "Late follower", &[9401, 9402], &[9401]).await;
        let authorized: HashSet<i32> = [9401].into_iter().collect();

        // B: 9402 starts following (as `set_inheritance` would), uncommitted.
        let b = conn.begin().await.unwrap();
        set_following(&b, 9402, true).await;

        let a = {
            let repos = repos.clone();
            let group_id = group.id;
            blocked("changing the repository", async move {
                repos
                    .apply_repository(
                        group_id,
                        &main_branch_repository(),
                        &authorized,
                        DeployCaller::from_instance_admin(false),
                    )
                    .await
            })
            .await
        };
        b.commit().await.unwrap();

        let err = done(a).await.unwrap_err();
        assert!(
            matches!(
                &err,
                ProjectGroupError::RepositoryFollowersChanged { group_id, project_ids }
                    if *group_id == group.id && project_ids == &vec![9402]
            ),
            "got {err:?}"
        );
        // Nothing moved: the group keeps its branch, both projects keep
        // theirs, and 9402 keeps the flag B gave it.
        assert_eq!(branch_of(&conn, group.id).await.as_deref(), Some("develop"));
        for id in [9401, 9402] {
            let project = projects::Entity::find_by_id(id)
                .one(conn.as_ref())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(project.main_branch, "main");
        }
        assert_eq!(following(&conn, group.id).await, vec![9401, 9402]);
    }

    #[tokio::test]
    async fn the_authorized_followers_are_written_through() {
        let Ok(db) = TestDatabase::with_migrations().await else {
            eprintln!("Docker/TimescaleDB unavailable, skipping repository test");
            return;
        };
        let conn = db.connection_arc();
        let Some(repos) = repository_service(conn.clone()).await else {
            return;
        };
        let group =
            group_with_followers(&conn, "Authorized", &[9411, 9412, 9413], &[9411, 9412]).await;
        let authorized: HashSet<i32> = [9411, 9412].into_iter().collect();

        let (updated, results) = repos
            .apply_repository(
                group.id,
                &main_branch_repository(),
                &authorized,
                DeployCaller::from_instance_admin(false),
            )
            .await
            .expect("every follower was authorized");
        assert_eq!(updated.main_branch.as_deref(), Some("main"));
        assert_eq!(branch_of(&conn, group.id).await.as_deref(), Some("main"));
        let ids: Vec<i32> = results.iter().map(|r| r.project_id).collect();
        assert_eq!(ids, vec![9411, 9412], "non-followers are not reported");
        assert!(results.iter().all(|r| r.error.is_none()), "{results:?}");
    }

    #[tokio::test]
    async fn a_follower_that_leaves_after_authorization_is_tolerated() {
        use sea_orm::TransactionTrait;
        let Ok(db) = TestDatabase::with_migrations().await else {
            eprintln!("Docker/TimescaleDB unavailable, skipping repository test");
            return;
        };
        let conn = db.connection_arc();
        let Some(repos) = repository_service(conn.clone()).await else {
            return;
        };
        let repos = Arc::new(repos);
        let group = group_with_followers(&conn, "Leaver", &[9421, 9422], &[9421, 9422]).await;
        let authorized: HashSet<i32> = [9421, 9422].into_iter().collect();

        // B: 9422 stops following, uncommitted.
        let b = conn.begin().await.unwrap();
        set_following(&b, 9422, false).await;
        let a = {
            let repos = repos.clone();
            let group_id = group.id;
            blocked("changing the repository", async move {
                repos
                    .apply_repository(
                        group_id,
                        &main_branch_repository(),
                        &authorized,
                        DeployCaller::from_instance_admin(false),
                    )
                    .await
            })
            .await
        };
        b.commit().await.unwrap();

        let (_, results) = done(a)
            .await
            .expect("a follower that left is not a conflict");
        let ids: Vec<i32> = results.iter().map(|r| r.project_id).collect();
        assert_eq!(ids, vec![9421]);
        assert_eq!(branch_of(&conn, group.id).await.as_deref(), Some("main"));
        assert_eq!(following(&conn, group.id).await, vec![9421]);
    }

    #[tokio::test]
    async fn clearing_with_a_follower_that_appeared_after_authorization_changes_nothing() {
        use sea_orm::TransactionTrait;
        let Ok(db) = TestDatabase::with_migrations().await else {
            eprintln!("Docker/TimescaleDB unavailable, skipping repository test");
            return;
        };
        let conn = db.connection_arc();
        let Some(repos) = repository_service(conn.clone()).await else {
            return;
        };
        let repos = Arc::new(repos);
        let group = group_with_followers(&conn, "Late clear", &[9431, 9432], &[9431]).await;
        let authorized: HashSet<i32> = [9431].into_iter().collect();

        let b = conn.begin().await.unwrap();
        set_following(&b, 9432, true).await;
        let a = {
            let repos = repos.clone();
            let group_id = group.id;
            let authorized = authorized.clone();
            blocked("clearing the repository", async move {
                repos.clear_repository(group_id, &authorized).await
            })
            .await
        };
        b.commit().await.unwrap();

        let err = done(a).await.unwrap_err();
        assert!(
            matches!(
                &err,
                ProjectGroupError::RepositoryFollowersChanged { project_ids, .. }
                    if project_ids == &vec![9432]
            ),
            "got {err:?}"
        );
        assert_eq!(branch_of(&conn, group.id).await.as_deref(), Some("develop"));
        assert_eq!(following(&conn, group.id).await, vec![9431, 9432]);

        // Checked on both followers, the same call goes through.
        let both: HashSet<i32> = [9431, 9432].into_iter().collect();
        let cleared = repos
            .clear_repository(group.id, &both)
            .await
            .expect("clear");
        assert!(cleared.main_branch.is_none());
        assert!(following(&conn, group.id).await.is_empty());
    }

    #[tokio::test]
    async fn following_waits_for_a_concurrent_repository_change_and_does_not_follow_a_stale_one() {
        use sea_orm::TransactionTrait;
        let Ok(db) = TestDatabase::with_migrations().await else {
            eprintln!("Docker/TimescaleDB unavailable, skipping repository test");
            return;
        };
        let conn = db.connection_arc();
        let Some(repos) = repository_service(conn.clone()).await else {
            return;
        };
        let repos = Arc::new(repos);
        // The member already points at acme/shop@main and the group is
        // moved to main by A, so following it writes nothing through.
        let group = group_with_followers(&conn, "Stale", &[9441], &[]).await;
        project_groups::Entity::update_many()
            .col_expr(
                project_groups::Column::MainBranch,
                sea_orm::sea_query::Expr::value("main"),
            )
            .filter(project_groups::Column::Id.eq(group.id))
            .exec(conn.as_ref())
            .await
            .unwrap();

        // B moves the group to another branch and has not committed.
        let b = conn.begin().await.unwrap();
        b.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE project_groups SET main_branch = 'release' WHERE id = $1",
            [group.id.into()],
        ))
        .await
        .unwrap();

        let a = {
            let repos = repos.clone();
            let group_id = group.id;
            blocked("following the repository", async move {
                repos
                    .set_inheritance(
                        group_id,
                        9441,
                        true,
                        DeployCaller::from_instance_admin(false),
                    )
                    .await
            })
            .await
        };
        b.commit().await.unwrap();

        let err = done(a).await.unwrap_err();
        assert!(
            matches!(
                err,
                ProjectGroupError::RepositoryChanged { group_id, project_id }
                    if group_id == group.id && project_id == 9441
            ),
            "got {err:?}"
        );
        assert!(following(&conn, group.id).await.is_empty());
    }

    #[tokio::test]
    async fn following_does_not_mark_a_project_that_moved_to_another_group() {
        use sea_orm::TransactionTrait;
        let Ok(db) = TestDatabase::with_migrations().await else {
            eprintln!("Docker/TimescaleDB unavailable, skipping repository test");
            return;
        };
        let conn = db.connection_arc();
        let Some(repos) = repository_service(conn.clone()).await else {
            return;
        };
        let repos = Arc::new(repos);
        let svc = ProjectGroupService::new(conn.clone());
        let group = group_with_followers(&conn, "Origin", &[9451], &[]).await;
        project_groups::Entity::update_many()
            .col_expr(
                project_groups::Column::MainBranch,
                sea_orm::sea_query::Expr::value("main"),
            )
            .filter(project_groups::Column::Id.eq(group.id))
            .exec(conn.as_ref())
            .await
            .unwrap();
        let elsewhere = svc
            .create_group(CreateProjectGroupRequest {
                name: "Elsewhere".into(),
                slug: None,
                description: None,
                project_ids: vec![],
            })
            .await
            .unwrap()
            .group
            .id;

        // B moves the project to another group and has not committed.
        let b = conn.begin().await.unwrap();
        ProjectGroupService::lock_projects(&b, &[9451])
            .await
            .unwrap();
        b.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE project_group_members SET group_id = $1 WHERE project_id = 9451",
            [elsewhere.into()],
        ))
        .await
        .unwrap();

        let a = {
            let repos = repos.clone();
            let group_id = group.id;
            blocked("following the repository", async move {
                repos
                    .set_inheritance(
                        group_id,
                        9451,
                        true,
                        DeployCaller::from_instance_admin(false),
                    )
                    .await
            })
            .await
        };
        b.commit().await.unwrap();

        let err = done(a).await.unwrap_err();
        assert!(
            matches!(
                err,
                ProjectGroupError::NotAMember { group_id, project_id }
                    if group_id == group.id && project_id == 9451
            ),
            "got {err:?}"
        );
        assert!(following(&conn, elsewhere).await.is_empty());
    }

    #[test]
    fn a_repository_matches_only_the_group_row_it_was_read_from() {
        let mut g = group(Some(7), Some(("acme", "shop", "main")));
        let repo = GroupRepository {
            git_provider_connection_id: Some(7),
            public_provider: None,
            repo_owner: "acme".into(),
            repo_name: "shop".into(),
            main_branch: "main".into(),
            repository_id: None,
            clone_url: None,
        };
        assert!(is_repository_of(&g, &repo));
        g.main_branch = Some("develop".into());
        assert!(!is_repository_of(&g, &repo));
        g.main_branch = Some("main".into());
        g.public_provider = Some("github".into());
        assert!(!is_repository_of(&g, &repo));
        g.public_provider = None;
        g.git_provider_connection_id = None;
        assert!(!is_repository_of(&g, &repo));
    }
}
