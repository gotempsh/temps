// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Configuration-time check that a project's root directory exists in its
//! repository at the configured branch.
//!
//! A directory missing from the checkout used to surface only at deploy time,
//! after the clone succeeded, as a bare `No such file or directory` from the
//! build step. This walks the repository through the provider's one-level
//! directory listing so the save that introduces the mistake can be refused
//! with the repository, branch and missing path named.
//!
//! ## Policy: refuse only on proof of absence
//!
//! The check refuses ([`RepositoryDirectoryCheck::Missing`]) only when a
//! directory it already confirmed exists was listed in full and has no entry
//! with the next component's name. Everything else is
//! [`RepositoryDirectoryCheck::Unverified`] and must not block the save:
//! the provider being unreachable, rate limited or rejecting the credential,
//! a provider without a listing API, an unknown branch, a listing that may be
//! truncated, or a component that exists but is not a plain directory (a
//! symlink to a directory is valid in a checkout, and listings do not say
//! where a link points). The deploy-time check in `download_repo` still
//! explains any such case precisely, so degrading here never hides a mistake;
//! it only moves the explanation to the first deploy.

use std::fmt::Display;
use std::future::Future;
use std::time::Duration;

/// Upper bound on a whole check, so a slow provider delays a save by at most
/// this long before the save proceeds unverified.
pub const REPOSITORY_DIRECTORY_CHECK_TIMEOUT: Duration = Duration::from_secs(10);

/// Deepest root directory the walk follows; deeper paths are left to deploy.
pub const MAX_DIRECTORY_DEPTH: usize = 32;

/// A listing with at least this many entries may have been cut off by the
/// provider (GitLab is paged to 300 here, GitHub's contents API stops at
/// 1000), so a name missing from it proves nothing.
pub const POSSIBLY_TRUNCATED_LISTING: usize = 300;

/// How many sibling directories the explanation names.
const LISTED_DIRECTORIES: usize = 10;

/// One entry of a directory listing, as far as this check needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryListingEntry {
    /// Base name of the entry (`"api"`, not `"apps/api"`).
    pub name: String,
    /// `true` only for a plain directory (a tree); `false` for files,
    /// symlinks and submodules.
    pub is_dir: bool,
}

/// Outcome of checking a root directory against the repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepositoryDirectoryCheck {
    /// Every component was found as a directory.
    Present,
    /// A fully listed parent directory has no entry for the next component.
    Missing(MissingRepositoryDirectory),
    /// The check could not decide; the caller must accept the configuration.
    Unverified { reason: String },
}

/// Where the walk stopped, and what the parent directory does contain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingRepositoryDirectory {
    /// Repository-relative parent that was listed; empty for the root.
    pub parent: String,
    /// The component that is not in `parent`.
    pub missing: String,
    /// Directories in `parent`, sorted, at most [`LISTED_DIRECTORIES`].
    pub available_directories: Vec<String>,
    /// How many more directories `parent` has beyond those listed.
    pub more_directories: usize,
    /// An entry whose name differs from `missing` only by letter case.
    pub case_only_match: Option<String>,
}

impl MissingRepositoryDirectory {
    /// One sentence naming the missing component and what is there instead,
    /// e.g. `'apps' has no 'apii'; it contains: api, web`.
    pub fn explanation(&self) -> String {
        let parent = if self.parent.is_empty() {
            "the repository root".to_string()
        } else {
            format!("'{}'", self.parent)
        };
        let mut explanation = format!("{parent} has no '{}'", self.missing);
        if let Some(near) = &self.case_only_match {
            explanation.push_str(&format!(
                " (there is '{near}'; directory names are case-sensitive)"
            ));
        }
        if self.available_directories.is_empty() {
            explanation.push_str("; it has no subdirectories");
        } else {
            explanation.push_str(&format!(
                "; it contains: {}",
                self.available_directories.join(", ")
            ));
            if self.more_directories > 0 {
                explanation.push_str(&format!(" and {} more", self.more_directories));
            }
        }
        explanation
    }
}

/// Split a normalized, repository-relative directory into components.
///
/// `None` when the path is not something the walk should follow (a parent
/// reference, or deeper than [`MAX_DIRECTORY_DEPTH`]).
fn directory_components(directory: &str) -> Option<Vec<&str>> {
    let components: Vec<&str> = directory
        .split('/')
        .filter(|component| !component.is_empty() && *component != ".")
        .collect();
    if components.contains(&"..") || components.len() > MAX_DIRECTORY_DEPTH {
        return None;
    }
    Some(components)
}

/// Walk `directory` one component at a time using `list`, which returns the
/// entries of a repository-relative path (`""` is the root) at the
/// configured reference.
///
/// Issues at most one listing per component, stopping at the first component
/// that is missing or cannot be confirmed.
pub async fn check_repository_directory<F, Fut, E>(
    directory: &str,
    mut list: F,
) -> RepositoryDirectoryCheck
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<Vec<DirectoryListingEntry>, E>>,
    E: Display,
{
    let Some(components) = directory_components(directory) else {
        return RepositoryDirectoryCheck::Unverified {
            reason: format!(
                "'{directory}' is deeper than {MAX_DIRECTORY_DEPTH} levels or leaves the \
                 repository, so it is checked at deploy time"
            ),
        };
    };
    let mut parent = String::new();
    for component in components {
        let entries = match list(parent.clone()).await {
            Ok(entries) => entries,
            Err(error) => {
                let listed = if parent.is_empty() {
                    "the repository root".to_string()
                } else {
                    format!("'{parent}'")
                };
                return RepositoryDirectoryCheck::Unverified {
                    reason: format!("could not list {listed}: {error}"),
                };
            }
        };
        match entries.iter().find(|entry| entry.name == component) {
            Some(entry) if entry.is_dir => {}
            Some(_) => {
                return RepositoryDirectoryCheck::Unverified {
                    reason: format!(
                        "'{}' exists but is not a plain directory (a file, symlink or \
                         submodule), so it is checked in the checkout at deploy time",
                        join(&parent, component)
                    ),
                };
            }
            None if entries.len() >= POSSIBLY_TRUNCATED_LISTING => {
                return RepositoryDirectoryCheck::Unverified {
                    reason: format!(
                        "the listing of '{}' has {} entries and may be truncated by the provider",
                        if parent.is_empty() { "/" } else { &parent },
                        entries.len()
                    ),
                };
            }
            None => {
                let case_only_match = entries
                    .iter()
                    .find(|entry| entry.name.eq_ignore_ascii_case(component))
                    .map(|entry| entry.name.clone());
                let mut directories: Vec<String> = entries
                    .iter()
                    .filter(|entry| entry.is_dir)
                    .map(|entry| entry.name.clone())
                    .collect();
                directories.sort();
                let more_directories = directories.len().saturating_sub(LISTED_DIRECTORIES);
                directories.truncate(LISTED_DIRECTORIES);
                return RepositoryDirectoryCheck::Missing(MissingRepositoryDirectory {
                    parent,
                    missing: component.to_string(),
                    available_directories: directories,
                    more_directories,
                    case_only_match,
                });
            }
        }
        parent = join(&parent, component);
    }
    RepositoryDirectoryCheck::Present
}

fn join(parent: &str, component: &str) -> String {
    if parent.is_empty() {
        component.to_string()
    } else {
        format!("{parent}/{component}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    fn dir(name: &str) -> DirectoryListingEntry {
        DirectoryListingEntry {
            name: name.to_string(),
            is_dir: true,
        }
    }

    fn file(name: &str) -> DirectoryListingEntry {
        DirectoryListingEntry {
            name: name.to_string(),
            is_dir: false,
        }
    }

    /// A fake repository: listings by path, recording every path listed.
    struct FakeRepository {
        listings: HashMap<String, Result<Vec<DirectoryListingEntry>, String>>,
        listed: Mutex<Vec<String>>,
    }

    impl FakeRepository {
        fn new(listings: Vec<(&str, Result<Vec<DirectoryListingEntry>, String>)>) -> Self {
            Self {
                listings: listings
                    .into_iter()
                    .map(|(path, listing)| (path.to_string(), listing))
                    .collect(),
                listed: Mutex::new(Vec::new()),
            }
        }

        async fn check(&self, directory: &str) -> RepositoryDirectoryCheck {
            check_repository_directory(directory, |path| {
                self.listed.lock().expect("listed").push(path.clone());
                let listing = self
                    .listings
                    .get(&path)
                    .cloned()
                    .unwrap_or_else(|| Err(format!("HTTP 404 for '{path}'")));
                async move { listing }
            })
            .await
        }

        fn listed(&self) -> Vec<String> {
            self.listed.lock().expect("listed").clone()
        }
    }

    fn monorepo() -> FakeRepository {
        FakeRepository::new(vec![
            (
                "",
                Ok(vec![
                    dir("apps"),
                    dir("examples"),
                    file("README.md"),
                    file("package.json"),
                ]),
            ),
            (
                "apps",
                Ok(vec![
                    dir("api"),
                    dir("web"),
                    dir("Admin"),
                    file("notes.txt"),
                ]),
            ),
            ("apps/api", Ok(vec![file("go.mod"), file("main.go")])),
        ])
    }

    #[tokio::test]
    async fn an_existing_nested_directory_is_present() {
        let repo = monorepo();
        assert_eq!(
            repo.check("apps/api").await,
            RepositoryDirectoryCheck::Present
        );
        // One listing per component, never the leaf's own contents.
        assert_eq!(repo.listed(), vec!["".to_string(), "apps".to_string()]);
    }

    #[tokio::test]
    async fn the_root_needs_no_listing() {
        let repo = monorepo();
        for root in ["", ".", "./"] {
            assert_eq!(repo.check(root).await, RepositoryDirectoryCheck::Present);
        }
        assert!(repo.listed().is_empty());
    }

    #[tokio::test]
    async fn a_missing_component_is_named_with_its_parent_and_siblings() {
        let repo = monorepo();
        let RepositoryDirectoryCheck::Missing(missing) = repo.check("apps/apii/src").await else {
            panic!("expected Missing");
        };
        assert_eq!(missing.parent, "apps");
        assert_eq!(missing.missing, "apii");
        assert_eq!(missing.available_directories, vec!["Admin", "api", "web"]);
        assert_eq!(missing.case_only_match, None);
        assert_eq!(
            missing.explanation(),
            "'apps' has no 'apii'; it contains: Admin, api, web"
        );
        // The walk stops at the first missing component.
        assert_eq!(repo.listed(), vec!["".to_string(), "apps".to_string()]);
    }

    #[tokio::test]
    async fn a_missing_top_level_directory_names_the_repository_root() {
        let repo = monorepo();
        let RepositoryDirectoryCheck::Missing(missing) = repo.check("services/api").await else {
            panic!("expected Missing");
        };
        assert_eq!(missing.parent, "");
        assert_eq!(
            missing.explanation(),
            "the repository root has no 'services'; it contains: apps, examples"
        );
    }

    #[tokio::test]
    async fn a_case_only_difference_is_called_out() {
        let repo = monorepo();
        let RepositoryDirectoryCheck::Missing(missing) = repo.check("apps/admin").await else {
            panic!("expected Missing");
        };
        assert_eq!(missing.case_only_match.as_deref(), Some("Admin"));
        assert!(
            missing
                .explanation()
                .contains("there is 'Admin'; directory names are case-sensitive"),
            "{}",
            missing.explanation()
        );
    }

    #[tokio::test]
    async fn a_parent_without_subdirectories_says_so() {
        let repo = monorepo();
        let RepositoryDirectoryCheck::Missing(missing) = repo.check("apps/api/cmd").await else {
            panic!("expected Missing");
        };
        assert_eq!(
            missing.explanation(),
            "'apps/api' has no 'cmd'; it has no subdirectories"
        );
    }

    #[tokio::test]
    async fn long_sibling_lists_are_capped() {
        let mut root: Vec<DirectoryListingEntry> =
            (0..25).map(|i| dir(&format!("pkg-{i:02}"))).collect();
        root.push(file("README.md"));
        let repo = FakeRepository::new(vec![("", Ok(root))]);
        let RepositoryDirectoryCheck::Missing(missing) = repo.check("app").await else {
            panic!("expected Missing");
        };
        assert_eq!(missing.available_directories.len(), 10);
        assert_eq!(missing.more_directories, 15);
        assert!(missing.explanation().ends_with("pkg-09 and 15 more"));
    }

    #[tokio::test]
    async fn provider_failures_never_refuse() {
        // Root listing fails (unreachable, rate limited, unknown branch...).
        let unreachable = FakeRepository::new(vec![("", Err("connection reset".to_string()))]);
        let RepositoryDirectoryCheck::Unverified { reason } = unreachable.check("apps/api").await
        else {
            panic!("expected Unverified");
        };
        assert!(
            reason.contains("could not list the repository root"),
            "{reason}"
        );
        assert!(reason.contains("connection reset"), "{reason}");

        // An intermediate directory that exists but cannot be listed.
        let flaky = FakeRepository::new(vec![
            ("", Ok(vec![dir("apps")])),
            ("apps", Err("HTTP 502".to_string())),
        ]);
        let RepositoryDirectoryCheck::Unverified { reason } = flaky.check("apps/api").await else {
            panic!("expected Unverified");
        };
        assert!(reason.contains("could not list 'apps'"), "{reason}");
    }

    #[tokio::test]
    async fn non_directories_and_possibly_truncated_listings_are_unverified() {
        // A symlink or submodule may well be a valid directory in the checkout.
        let linked = FakeRepository::new(vec![("", Ok(vec![file("apps")]))]);
        assert!(matches!(
            linked.check("apps/api").await,
            RepositoryDirectoryCheck::Unverified { .. }
        ));

        let huge: Vec<DirectoryListingEntry> = (0..POSSIBLY_TRUNCATED_LISTING)
            .map(|i| dir(&format!("d{i}")))
            .collect();
        let truncated = FakeRepository::new(vec![("", Ok(huge))]);
        let RepositoryDirectoryCheck::Unverified { reason } = truncated.check("zzz").await else {
            panic!("expected Unverified");
        };
        assert!(reason.contains("may be truncated"), "{reason}");
    }

    #[tokio::test]
    async fn paths_the_walk_should_not_follow_are_unverified_without_listing() {
        let repo = monorepo();
        let deep = vec!["d"; MAX_DIRECTORY_DEPTH + 1].join("/");
        for directory in ["apps/../secrets", deep.as_str()] {
            assert!(matches!(
                repo.check(directory).await,
                RepositoryDirectoryCheck::Unverified { .. }
            ));
        }
        assert!(repo.listed().is_empty());
    }
}
