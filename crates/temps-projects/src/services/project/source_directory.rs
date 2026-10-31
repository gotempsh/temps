// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Validation of a project's source root directory.

use super::*;

pub(super) fn normalize_project_directory(directory: &str) -> Result<String, ProjectError> {
    let normalized = directory
        .trim()
        .replace('\\', "/")
        .trim_start_matches('/')
        .to_string();
    if normalized.is_empty() || normalized == "." {
        return Ok(".".to_string());
    }
    let path = std::path::Path::new(&normalized);
    let has_windows_drive_prefix = normalized.as_bytes().get(1) == Some(&b':');
    if has_windows_drive_prefix
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(ProjectError::InvalidInput(format!(
            "Project directory '{directory}' must be a relative path inside the source root"
        )));
    }
    Ok(normalized.trim_start_matches("./").to_string())
}

/// The source location a write would leave a Git project with, as far as the
/// configuration-time root directory check (issue #1350) needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SourceDirectoryTarget<'a> {
    pub(super) connection_id: Option<i32>,
    pub(super) is_public_repo: bool,
    pub(super) git_url: Option<&'a str>,
    pub(super) repo_owner: &'a str,
    pub(super) repo_name: &'a str,
    pub(super) branch: &'a str,
    /// Already normalized with [`normalize_project_directory`].
    pub(super) directory: &'a str,
}

impl SourceDirectoryTarget<'_> {
    /// Whether this write changes anything the check depends on compared with
    /// the stored project. Unchanged sources are not re-checked, so an
    /// unrelated edit (a preset change re-sending the same directory) is never
    /// refused because the repository changed underneath the project.
    pub(super) fn differs_from(&self, project: &projects::Model) -> bool {
        // The public flag and URL decide whether (and against which
        // provider) a connectionless project can be checked at all.
        self.connection_id != project.git_provider_connection_id
            || self.is_public_repo != project.is_public_repo
            || self.git_url != project.git_url.as_deref()
            || self.repo_owner != project.repo_owner
            || self.repo_name != project.repo_name
            || self.branch != project.main_branch
            || self.directory != normalized_stored_directory(&project.directory)
    }
}

/// A stored directory in the normalized form, for change detection. Rows
/// written before normalization (`"/"`, `"./"`) compare equal to `"."`.
pub(super) fn normalized_stored_directory(directory: &str) -> String {
    normalize_project_directory(directory).unwrap_or_else(|_| directory.to_string())
}

/// The public provider that can list `git_url`'s repository without
/// credentials, when there is one.
///
/// Only GitHub: its contents API answers unauthenticated requests for public
/// repositories. Deliberately unauthenticated, so the check can never reveal
/// anything about a private repository a shared credential could see.
fn public_directory_provider(git_url: Option<&str>) -> Option<&'static str> {
    let url = url::Url::parse(git_url?.trim()).ok()?;
    if !matches!(url.scheme(), "https" | "http") {
        return None;
    }
    match url.host_str()?.to_ascii_lowercase().as_str() {
        "github.com" | "www.github.com" => Some("github"),
        _ => None,
    }
}

/// Root checkout (`.`) cannot use sparse clone — there is no subdirectory.
pub(super) fn pull_only_root_directory_value(
    normalized_directory: &str,
    requested: Option<bool>,
    existing: bool,
) -> bool {
    if normalized_directory == "." {
        return false;
    }
    requested.unwrap_or(existing)
}

impl ProjectService {
    /// Refuse a root directory the repository provably does not have at the
    /// configured branch (issue #1350).
    ///
    /// Policy: refuse only on proof of absence (see
    /// `temps_git::services::repository_directory`). A provider that cannot
    /// be reached, rate limits, has no listing API, or a project without a
    /// connection or a listable public URL, saves unverified with a warning:
    /// GitHub being flaky must never stop someone saving a project, and the
    /// deploy-time check in `download_repo` still explains a missing
    /// directory against the actual checkout. Bounded by the provider check's
    /// own timeout, and at most one listing per path component.
    pub(super) async fn verify_root_directory_in_repository(
        &self,
        project_id: Option<i32>,
        target: SourceDirectoryTarget<'_>,
    ) -> Result<(), ProjectError> {
        use temps_git::RepositoryDirectoryCheck;

        let owner = target.repo_owner.trim();
        let name = target.repo_name.trim();
        let branch = target.branch.trim();
        if target.directory == "."
            || owner.is_empty()
            || name.is_empty()
            || owner == "unknown"
            || name == "unknown"
            || branch.is_empty()
        {
            return Ok(());
        }
        let repository = format!("{owner}/{name}");
        let check = if let Some(connection_id) = target.connection_id.filter(|id| *id > 0) {
            self.git_provider_manager
                .check_repository_directory(connection_id, owner, name, branch, target.directory)
                .await
        } else if target.is_public_repo {
            match public_directory_provider(target.git_url) {
                Some(provider_name) => match PublicRepoProviderFactory::create(provider_name) {
                    Ok(provider) => {
                        temps_git::services::public_repo::check_public_repository_directory(
                            provider.as_ref(),
                            owner,
                            name,
                            branch,
                            target.directory,
                        )
                        .await
                    }
                    Err(error) => RepositoryDirectoryCheck::Unverified {
                        reason: format!("the {provider_name} public API is unavailable: {error}"),
                    },
                },
                None => RepositoryDirectoryCheck::Unverified {
                    reason: "only public GitHub repositories can be listed without a Git \
                             connection"
                        .to_string(),
                },
            }
        } else {
            RepositoryDirectoryCheck::Unverified {
                reason: "the project has no Git connection to list the repository with".to_string(),
            }
        };
        match check {
            RepositoryDirectoryCheck::Present => {
                info!(
                    project_id = ?project_id,
                    %repository,
                    branch,
                    directory = target.directory,
                    "Root directory '{}' exists in {} on branch '{}'",
                    target.directory,
                    repository,
                    branch
                );
                Ok(())
            }
            RepositoryDirectoryCheck::Missing(missing) => {
                warn!(
                    project_id = ?project_id,
                    %repository,
                    branch,
                    directory = target.directory,
                    "Refusing root directory '{}': {}",
                    target.directory,
                    missing.explanation()
                );
                Err(ProjectError::DirectoryNotInRepository {
                    repository,
                    branch: branch.to_string(),
                    directory: target.directory.to_string(),
                    explanation: missing.explanation(),
                })
            }
            RepositoryDirectoryCheck::Unverified { reason } => {
                warn!(
                    project_id = ?project_id,
                    %repository,
                    branch,
                    directory = target.directory,
                    %reason,
                    "Saving root directory '{}' for {} on branch '{}' without verifying it \
                     exists ({}); the first deployment checks it in the checkout",
                    target.directory,
                    repository,
                    branch,
                    reason
                );
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_public_github_https_urls_are_listed_without_a_connection() {
        for (url, expected) in [
            ("https://github.com/example/monorepo", Some("github")),
            ("https://github.com/example/monorepo.git", Some("github")),
            ("https://WWW.GitHub.com/example/monorepo", Some("github")),
            ("https://gitlab.com/example/monorepo", None),
            ("https://github.com.example.net/example/monorepo", None),
            ("git@github.com:example/monorepo.git", None),
            ("ssh://git@github.com/example/monorepo", None),
            ("not a url", None),
        ] {
            assert_eq!(public_directory_provider(Some(url)), expected, "{url}");
        }
        assert_eq!(public_directory_provider(None), None);
    }

    #[test]
    fn pull_only_root_directory_is_forced_off_at_repository_root() {
        assert!(!pull_only_root_directory_value(".", Some(true), true));
        assert!(!pull_only_root_directory_value(".", None, true));
    }

    #[test]
    fn pull_only_root_directory_keeps_existing_when_omitted() {
        assert!(pull_only_root_directory_value("apps/web", None, true));
        assert!(!pull_only_root_directory_value("apps/web", None, false));
        assert!(pull_only_root_directory_value(
            "apps/web",
            Some(true),
            false
        ));
        assert!(!pull_only_root_directory_value(
            "apps/web",
            Some(false),
            true
        ));
    }

    #[test]
    fn project_directory_must_remain_inside_source_root() {
        assert_eq!(normalize_project_directory("").unwrap(), ".");
        assert_eq!(
            normalize_project_directory("./apps/web").unwrap(),
            "apps/web"
        );
        assert!(matches!(
            normalize_project_directory("../secrets"),
            Err(ProjectError::InvalidInput(_))
        ));
        assert_eq!(
            normalize_project_directory("/apps/web").unwrap(),
            "apps/web"
        );
        assert!(matches!(
            normalize_project_directory("apps/../../etc"),
            Err(ProjectError::InvalidInput(_))
        ));
    }
}
