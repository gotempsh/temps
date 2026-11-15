// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Checks on caller-supplied repository names, branches and commit contents,
//! run before any credentialed call reaches a git provider.

use super::*;

/// Longest repository name the supported providers accept.
pub(crate) const MAX_REPOSITORY_NAME_LEN: usize = 100;

/// Longest description GitHub accepts; the strictest of the supported providers.
const MAX_REPOSITORY_DESCRIPTION_LEN: usize = 350;

/// Check the caller-supplied parts of a new repository before they are sent,
/// with a user's token, to a git provider.
///
/// Names and owners are limited to the characters every supported provider
/// accepts, so nothing path-like (`..`, a leading `/`) or control-character
/// bearing reaches a provider URL or request body.
pub(super) fn validate_new_repository(
    connection_id: i32,
    repo_name: &str,
    repo_owner: Option<&str>,
    description: Option<&str>,
) -> Result<(), GitProviderManagerError> {
    let invalid = |reason: String| GitProviderManagerError::InvalidConfiguration(reason);
    let segment_ok = is_safe_name_segment;

    if repo_name.len() > MAX_REPOSITORY_NAME_LEN || !segment_ok(repo_name) {
        return Err(invalid(format!(
            "Repository name '{repo_name}' (connection {connection_id}) must be 1-{MAX_REPOSITORY_NAME_LEN} characters of letters, digits, '-', '_' or '.'"
        )));
    }
    if repo_name.ends_with(".git") {
        return Err(invalid(format!(
            "Repository name '{repo_name}' (connection {connection_id}) must not end in '.git'"
        )));
    }
    if let Some(owner) = repo_owner {
        // GitLab namespaces may be nested groups (`group/subgroup`).
        if owner.len() > MAX_REPOSITORY_NAME_LEN || !owner.split('/').all(segment_ok) {
            return Err(invalid(format!(
                "Repository owner '{owner}' (connection {connection_id}) must be an account or group name of letters, digits, '-', '_' or '.', optionally nested with '/'"
            )));
        }
    }
    if let Some(description) = description {
        if description.chars().count() > MAX_REPOSITORY_DESCRIPTION_LEN
            || description.chars().any(char::is_control)
        {
            return Err(invalid(format!(
                "Repository description for '{repo_name}' (connection {connection_id}) must be at most {MAX_REPOSITORY_DESCRIPTION_LEN} characters with no control characters"
            )));
        }
    }
    Ok(())
}

/// Whether `segment` is an account, group or repository name every supported
/// provider accepts: letters, digits, `-`, `_` and `.`, and never `.`/`..`.
pub(crate) fn is_safe_name_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Most files one commit may carry.
pub const MAX_COMMIT_FILES: usize = 100;

/// Most file content, in bytes, one commit may carry.
pub const MAX_COMMIT_BYTES: usize = 5 * 1024 * 1024;

const MAX_COMMIT_PATH_LEN: usize = 255;

const MAX_COMMIT_MESSAGE_LEN: usize = 500;

const MAX_BRANCH_NAME_LEN: usize = 200;

pub(super) const MAX_PULL_REQUEST_TITLE_LEN: usize = 256;

pub(super) const MAX_PULL_REQUEST_BODY_LEN: usize = 20_000;

/// How long a clone + commit + push may take before the caller gets an error.
pub(super) const COMMIT_FILES_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Check a branch name before it reaches a git ref or a provider URL.
pub(super) fn validate_branch_name(
    connection_id: i32,
    repository: &str,
    branch: &str,
) -> Result<(), GitProviderManagerError> {
    let bad = branch.is_empty()
        || branch.len() > MAX_BRANCH_NAME_LEN
        || branch.starts_with(['/', '.', '-'])
        || branch.ends_with(['/', '.'])
        || branch.ends_with(".lock")
        || branch.contains("..")
        || branch.contains("//")
        || branch.contains("@{")
        || !branch
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'));
    if bad {
        return Err(GitProviderManagerError::InvalidConfiguration(format!(
            "Branch '{branch}' for repository '{repository}' (connection {connection_id}) must be 1-{MAX_BRANCH_NAME_LEN} characters of letters, digits, '-', '_', '.' or '/', and must not start with '/', '.' or '-', end with '/', '.' or '.lock', or contain '..', '//' or '@{{'"
        )));
    }
    Ok(())
}

/// Check the owner/name pair of an existing repository.
pub(super) fn validate_repository_ref(
    connection_id: i32,
    owner: &str,
    repo: &str,
) -> Result<(), GitProviderManagerError> {
    if owner.len() > MAX_REPOSITORY_NAME_LEN
        || !owner.split('/').all(is_safe_name_segment)
        || repo.len() > MAX_REPOSITORY_NAME_LEN
        || !is_safe_name_segment(repo)
    {
        return Err(GitProviderManagerError::InvalidConfiguration(format!(
            "Repository '{owner}/{repo}' (connection {connection_id}) must be an owner and a name of letters, digits, '-', '_' or '.'"
        )));
    }
    Ok(())
}

/// Check everything a caller supplies to a commit before any credentialed
/// call: paths must stay inside the repository and out of `.git`, and the
/// whole commit is bounded in file count and bytes.
/// Whether `revision` is a full SHA-1 or SHA-256 commit id.
pub(super) fn is_commit_id(revision: &str) -> bool {
    matches!(revision.len(), 40 | 64) && revision.bytes().all(|b| b.is_ascii_hexdigit())
}

pub(super) fn validate_commit_request(
    connection_id: i32,
    owner: &str,
    repo: &str,
    branch: Option<&str>,
    files: &[(String, Vec<u8>)],
    message: &str,
) -> Result<(), GitProviderManagerError> {
    let invalid = |reason: String| GitProviderManagerError::InvalidConfiguration(reason);
    let repository = format!("{owner}/{repo}");
    validate_repository_ref(connection_id, owner, repo)?;
    if let Some(branch) = branch {
        validate_branch_name(connection_id, &repository, branch)?;
    }
    if message.trim().is_empty() || message.len() > MAX_COMMIT_MESSAGE_LEN || message.contains('\0')
    {
        return Err(invalid(format!(
            "Commit message for '{repository}' (connection {connection_id}) must be 1-{MAX_COMMIT_MESSAGE_LEN} bytes with no NUL characters"
        )));
    }
    if files.is_empty() || files.len() > MAX_COMMIT_FILES {
        return Err(invalid(format!(
            "A commit to '{repository}' (connection {connection_id}) must contain 1-{MAX_COMMIT_FILES} files, got {}",
            files.len()
        )));
    }
    let total: usize = files.iter().map(|(_, content)| content.len()).sum();
    if total > MAX_COMMIT_BYTES {
        return Err(invalid(format!(
            "A commit to '{repository}' (connection {connection_id}) carries {total} bytes; the limit is {MAX_COMMIT_BYTES}"
        )));
    }
    let mut seen = std::collections::HashSet::with_capacity(files.len());
    for (path, _) in files {
        let bad_segment = |segment: &str| {
            segment.is_empty()
                || segment == "."
                || segment == ".."
                || segment.eq_ignore_ascii_case(".git")
        };
        if path.len() > MAX_COMMIT_PATH_LEN
            || path.starts_with('/')
            || path.contains('\\')
            || path.chars().any(char::is_control)
            || path.split('/').any(bad_segment)
        {
            return Err(invalid(format!(
                "File path '{}' in a commit to '{repository}' (connection {connection_id}) must be a relative path of at most {MAX_COMMIT_PATH_LEN} characters, without '..', empty or '.git' segments, backslashes or control characters",
                path.escape_debug()
            )));
        }
        if !seen.insert(path.as_str()) {
            return Err(invalid(format!(
                "File path '{path}' appears more than once in a commit to '{repository}' (connection {connection_id})"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::git_provider_manager::test_support::*;

    #[test]
    fn new_repository_accepts_ordinary_names_owners_and_descriptions() {
        assert!(validate_new_repository(1, "my-app_2.0", None, None).is_ok());
        assert!(validate_new_repository(1, "app", Some("acme"), Some("A demo")).is_ok());
        assert!(validate_new_repository(1, "app", Some("group/sub-group"), None).is_ok());
    }

    #[test]
    fn new_repository_rejects_names_that_could_reach_a_provider_url() {
        for name in ["", ".", "..", "a/b", "a b", "a?x=1", "naïve", "repo.git"] {
            let error =
                validate_new_repository(7, name, None, None).expect_err("name should be rejected");
            assert!(
                matches!(&error, GitProviderManagerError::InvalidConfiguration(reason)
                    if reason.contains("connection 7")),
                "unexpected error for {name:?}: {error}"
            );
        }
        let long = "a".repeat(MAX_REPOSITORY_NAME_LEN + 1);
        assert!(validate_new_repository(7, &long, None, None).is_err());
    }

    #[test]
    fn new_repository_rejects_bad_owners_and_descriptions() {
        for owner in ["", "/acme", "acme/", "acme//x", "../acme", "acme x"] {
            assert!(
                validate_new_repository(7, "app", Some(owner), None).is_err(),
                "owner {owner:?} should be rejected"
            );
        }
        assert!(validate_new_repository(7, "app", None, Some("line\nbreak")).is_err());
        let long = "d".repeat(MAX_REPOSITORY_DESCRIPTION_LEN + 1);
        assert!(validate_new_repository(7, "app", None, Some(&long)).is_err());
    }

    #[test]
    fn a_commit_accepts_ordinary_paths_branches_and_messages() {
        let files = vec![
            ("main.go".to_string(), b"package main".to_vec()),
            ("cmd/server/main.go".to_string(), b"x".to_vec()),
            (".github/workflows/ci.yml".to_string(), b"on: push".to_vec()),
            ("docs/.gitkeep".to_string(), Vec::new()),
        ];
        validate_commit_request(3, "acme", "app", None, &files, "Add starter")
            .expect("plain commit");
        validate_commit_request(3, "acme", "app", Some("feature/add-starter_1"), &files, "m")
            .expect("slashed branch");
    }

    #[test]
    fn a_commit_rejects_paths_that_could_leave_the_repository_or_touch_git_metadata() {
        for path in [
            "",
            "/etc/passwd",
            "../outside",
            "a/../../outside",
            "a//b",
            "a/./b",
            ".git/config",
            "src/.GIT/hooks/pre-commit",
            "a\\b",
            "bad\nname",
            "trailing/",
        ] {
            let result = validate_commit_request(3, "acme", "app", None, &one_file(path), "m");
            assert!(result.is_err(), "path {path:?} should be rejected");
        }
        let long = "a".repeat(MAX_COMMIT_PATH_LEN + 1);
        assert!(validate_commit_request(3, "acme", "app", None, &one_file(&long), "m").is_err());
    }

    #[test]
    fn a_commit_rejects_duplicate_paths_and_oversized_or_empty_file_sets() {
        let duplicated = vec![
            ("a.txt".to_string(), vec![1]),
            ("a.txt".to_string(), vec![2]),
        ];
        let error = validate_commit_request(3, "acme", "app", None, &duplicated, "m")
            .expect_err("duplicate path");
        assert!(error.to_string().contains("a.txt"));
        assert!(error.to_string().contains("connection 3"));

        assert!(validate_commit_request(3, "acme", "app", None, &[], "m").is_err());

        let too_many: Vec<_> = (0..=MAX_COMMIT_FILES)
            .map(|i| (format!("f{i}.txt"), vec![0u8]))
            .collect();
        assert!(validate_commit_request(3, "acme", "app", None, &too_many, "m").is_err());

        let too_big = vec![("big.bin".to_string(), vec![0u8; MAX_COMMIT_BYTES + 1])];
        assert!(validate_commit_request(3, "acme", "app", None, &too_big, "m").is_err());
    }

    #[test]
    fn a_commit_rejects_bad_messages_branches_and_repositories() {
        let files = one_file("a.txt");
        for message in ["", "   ", "nul\0byte"] {
            assert!(validate_commit_request(3, "acme", "app", None, &files, message).is_err());
        }
        let long = "m".repeat(MAX_COMMIT_MESSAGE_LEN + 1);
        assert!(validate_commit_request(3, "acme", "app", None, &files, &long).is_err());

        for branch in [
            "",
            "/main",
            "main/",
            ".hidden",
            "-flag",
            "a..b",
            "a//b",
            "a@{1}",
            "x.lock",
            "has space",
            "semi;colon",
            "end.",
        ] {
            assert!(
                validate_commit_request(3, "acme", "app", Some(branch), &files, "m").is_err(),
                "branch {branch:?} should be rejected"
            );
        }

        for (owner, repo) in [
            ("", "app"),
            ("acme", ""),
            ("..", "app"),
            ("acme", ".."),
            ("a b", "app"),
            ("acme", "a/b"),
        ] {
            assert!(
                validate_commit_request(3, owner, repo, None, &files, "m").is_err(),
                "{owner:?}/{repo:?} should be rejected"
            );
        }
    }

    #[test]
    fn only_full_hex_commit_ids_are_accepted_as_revisions() {
        assert!(is_commit_id("0123456789abcdef0123456789abcdef01234567"));
        assert!(is_commit_id(&"a".repeat(64)));
        assert!(!is_commit_id("0123456"));
        assert!(!is_commit_id("main"));
        assert!(!is_commit_id(&"g".repeat(40)));
    }
}
