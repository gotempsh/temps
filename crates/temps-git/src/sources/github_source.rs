// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! GitHub API implementation of ProjectSource
//!
//! This implementation fetches files directly from GitHub API on-demand

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use http_body_util::BodyExt;
use octocrab::Octocrab;
use std::sync::Arc;
use temps_presets::source::{ProjectSource, ProjectSourceError, ProjectSourceResult};
use tracing::debug;

const LIST_FILES: &str = "list the repository tree";

/// GitHub-backed ProjectSource
///
/// Fetches files directly from GitHub API without cloning the repository
pub struct GitHubSource {
    client: Arc<Octocrab>,
    owner: String,
    repo: String,
    reference: String, // branch, tag, or commit SHA
}

impl GitHubSource {
    /// Create a new GitHub source
    ///
    /// # Arguments
    /// * `client` - Octocrab client (authenticated)
    /// * `owner` - Repository owner
    /// * `repo` - Repository name
    /// * `reference` - Branch name, tag, or commit SHA
    pub fn new(client: Arc<Octocrab>, owner: String, repo: String, reference: String) -> Self {
        Self {
            client,
            owner,
            repo,
            reference,
        }
    }

    fn location(&self) -> String {
        format!(
            "GitHub repository {}/{} at '{}'",
            self.owner, self.repo, self.reference
        )
    }

    /// Classify an answer GitHub refused. GitHub reports a primary rate limit
    /// as a 403, so only the message tells it apart from missing access.
    fn status_error(
        &self,
        operation: String,
        path: Option<&str>,
        status: u16,
        message: &str,
    ) -> ProjectSourceError {
        if status == 403 && message.to_ascii_lowercase().contains("rate limit") {
            return ProjectSourceError::RateLimited {
                location: self.location(),
                operation,
                status,
            };
        }
        ProjectSourceError::from_status(self.location(), operation, path, status)
    }

    fn invalid_content(
        &self,
        path: &str,
        problem: &str,
        error: impl std::fmt::Display,
    ) -> ProjectSourceError {
        ProjectSourceError::InvalidContent {
            location: self.location(),
            path: path.to_string(),
            reason: format!("{problem}: {error}"),
        }
    }

    fn api_error(
        &self,
        operation: String,
        path: Option<&str>,
        error: octocrab::Error,
    ) -> ProjectSourceError {
        match &error {
            octocrab::Error::GitHub { source, .. } => self.status_error(
                operation,
                path,
                source.status_code.as_u16(),
                &source.message,
            ),
            _ => ProjectSourceError::Transport {
                location: self.location(),
                operation,
                reason: error.to_string(),
            },
        }
    }
}

#[async_trait]
impl ProjectSource for GitHubSource {
    async fn has_file(&self, path: &str) -> bool {
        debug!(
            "Checking if file exists: {}/{}/{} @ {}",
            self.owner, self.repo, path, self.reference
        );

        match self
            .client
            .repos(&self.owner, &self.repo)
            .get_content()
            .path(path)
            .r#ref(&self.reference)
            .send()
            .await
        {
            Ok(content) => {
                // Check if it's a file (not a directory)
                content.items.iter().any(|item| {
                    item.path == path && (item.r#type == "file" || item.r#type == "blob")
                })
            }
            Err(_) => false,
        }
    }

    async fn has_directory(&self, path: &str) -> bool {
        debug!(
            "Checking if directory exists: {}/{}/{} @ {}",
            self.owner, self.repo, path, self.reference
        );

        match self
            .client
            .repos(&self.owner, &self.repo)
            .get_content()
            .path(path)
            .r#ref(&self.reference)
            .send()
            .await
        {
            Ok(content) => {
                // Check if it's a directory
                content
                    .items
                    .iter()
                    .any(|item| item.path == path && item.r#type == "dir")
            }
            Err(_) => false,
        }
    }

    async fn read_file(&self, path: &str) -> ProjectSourceResult<String> {
        debug!(
            "Reading file: {}/{}/{} @ {}",
            self.owner, self.repo, path, self.reference
        );

        let content = self
            .client
            .repos(&self.owner, &self.repo)
            .get_content()
            .path(path)
            .r#ref(&self.reference)
            .send()
            .await
            .map_err(|e| self.api_error(format!("fetch '{path}'"), Some(path), e))?;

        // Get the first item (file content)
        let file = content
            .items
            .first()
            .ok_or_else(|| ProjectSourceError::NotFound {
                location: self.location(),
                path: path.to_string(),
            })?;

        // Decode base64 content
        if let Some(encoded_content) = &file.content {
            let decoded = STANDARD
                .decode(encoded_content.replace('\n', ""))
                .map_err(|e| self.invalid_content(path, "not valid base64", e))?;

            let content_str = String::from_utf8(decoded)
                .map_err(|e| self.invalid_content(path, "not valid UTF-8", e))?;

            Ok(content_str.replace("\r\n", "\n"))
        } else {
            Err(ProjectSourceError::MissingContent {
                location: self.location(),
                path: path.to_string(),
            })
        }
    }

    async fn list_files(&self) -> ProjectSourceResult<Vec<String>> {
        debug!(
            "Listing all files: {}/{} @ {}",
            self.owner, self.repo, self.reference
        );

        // Use the reference directly - GitHub API accepts branch names, tags, and commit SHAs
        let tree_sha = &self.reference;

        // Use GitHub REST API directly for recursive tree
        let url = format!(
            "https://api.github.com/repos/{}/{}/git/trees/{}?recursive=1",
            self.owner, self.repo, tree_sha
        );

        #[derive(serde::Deserialize)]
        struct TreeResponse {
            tree: Vec<TreeEntry>,
        }

        #[derive(serde::Deserialize)]
        struct TreeEntry {
            path: String,
            #[serde(rename = "type")]
            entry_type: String,
        }

        // Use octocrab's GET method and parse JSON
        let response = self
            .client
            ._get(&url)
            .await
            .map_err(|e| self.api_error(LIST_FILES.to_string(), None, e))?;
        let status = response.status();

        // Read response body and parse JSON
        let body = response
            .into_body()
            .collect()
            .await
            .map_err(|e| ProjectSourceError::Transport {
                location: self.location(),
                operation: LIST_FILES.to_string(),
                reason: e.to_string(),
            })?
            .to_bytes();

        if !status.is_success() {
            return Err(self.status_error(
                LIST_FILES.to_string(),
                None,
                status.as_u16(),
                &String::from_utf8_lossy(&body),
            ));
        }

        let tree_response: TreeResponse =
            serde_json::from_slice(&body).map_err(|e| ProjectSourceError::InvalidResponse {
                location: self.location(),
                operation: LIST_FILES.to_string(),
                reason: e.to_string(),
            })?;

        // Filter only files (blobs)
        let files: Vec<String> = tree_response
            .tree
            .into_iter()
            .filter(|entry| entry.entry_type == "blob")
            .map(|entry| entry.path)
            .collect();

        debug!("Found {} files", files.len());
        Ok(files)
    }

    async fn find_files(&self, pattern: &str) -> ProjectSourceResult<Vec<String>> {
        debug!(
            "Finding files matching pattern '{}': {}/{} @ {}",
            pattern, self.owner, self.repo, self.reference
        );

        let all_files = self.list_files().await?;

        // Convert glob pattern to regex
        let regex_pattern = glob_to_regex(pattern);
        let re = regex::Regex::new(&regex_pattern)
            .map_err(|e| ProjectSourceError::invalid_pattern(pattern, e))?;

        let matched_files: Vec<String> = all_files.into_iter().filter(|f| re.is_match(f)).collect();

        debug!(
            "Found {} files matching pattern '{}'",
            matched_files.len(),
            pattern
        );
        Ok(matched_files)
    }
}

/// Convert glob pattern to regex
fn glob_to_regex(pattern: &str) -> String {
    let mut regex = String::from("^");
    let mut chars = pattern.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '*' if chars.peek() == Some(&'*') => {
                chars.next(); // consume second *
                if chars.peek() == Some(&'/') {
                    chars.next(); // consume /
                    regex.push_str("(?:.*/)?");
                } else {
                    regex.push_str(".*");
                }
            }
            '*' => {
                regex.push_str("[^/]*");
            }
            '?' => {
                regex.push_str("[^/]");
            }
            '.' | '(' | ')' | '+' | '|' | '^' | '$' | '@' | '%' => {
                regex.push('\\');
                regex.push(ch);
            }
            _ => {
                regex.push(ch);
            }
        }
    }

    regex.push('$');
    regex
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_over(url: &str) -> GitHubSource {
        let client = Octocrab::builder()
            .base_uri(url)
            .expect("the mock server URL is valid")
            .build()
            .expect("the client builds");
        GitHubSource::new(
            Arc::new(client),
            "owner".to_string(),
            "repo".to_string(),
            "main".to_string(),
        )
    }

    async fn read_error(status: usize, body: &str) -> ProjectSourceError {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock(
                "GET",
                mockito::Matcher::Regex("/repos/owner/repo/contents/.*".to_string()),
            )
            .with_status(status)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;
        source_over(&server.url())
            .read_file("app.json")
            .await
            .expect_err("the provider refused the request")
    }

    #[tokio::test]
    async fn read_file_tells_not_found_from_auth_and_rate_limits() {
        let missing = read_error(404, r#"{"message":"Not Found"}"#).await;
        assert!(
            matches!(&missing, ProjectSourceError::NotFound { path, .. } if path == "app.json"),
            "{missing:?}"
        );
        assert!(!missing.is_retryable());

        let refused = read_error(401, r#"{"message":"Bad credentials"}"#).await;
        assert!(
            matches!(refused, ProjectSourceError::AuthFailed { status: 401, .. }),
            "{refused:?}"
        );

        let limited = read_error(403, r#"{"message":"API rate limit exceeded"}"#).await;
        assert!(
            matches!(limited, ProjectSourceError::RateLimited { status: 403, .. }),
            "{limited:?}"
        );
        assert!(limited.is_retryable());

        let forbidden = read_error(403, r#"{"message":"Resource not accessible"}"#).await;
        assert!(
            matches!(
                forbidden,
                ProjectSourceError::AuthFailed { status: 403, .. }
            ),
            "{forbidden:?}"
        );
    }

    #[test]
    fn test_glob_to_regex() {
        assert_eq!(glob_to_regex("*.json"), r"^[^/]*\.json$");
        assert_eq!(glob_to_regex("**/*.ts"), r"^(?:.*/)?[^/]*\.ts$");
        assert_eq!(glob_to_regex("src/**/*.tsx"), r"^src/(?:.*/)?[^/]*\.tsx$");
        assert_eq!(glob_to_regex("test?.js"), r"^test[^/]\.js$");
    }
}
