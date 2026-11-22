// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! GitLab API implementation of ProjectSource
//!
//! This implementation fetches files directly from GitLab API on-demand

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Deserialize;
use std::sync::Arc;
use temps_presets::source::{ProjectSource, ProjectSourceError, ProjectSourceResult};
use tracing::{debug, warn};

#[derive(Debug, Deserialize)]
struct GitLabTreeEntry {
    #[serde(rename = "type")]
    entry_type: String,
    path: String,
}

#[derive(Debug, Deserialize)]
struct GitLabFileContent {
    encoding: String,
    content: String,
}

const LIST_FILES: &str = "list the repository tree";

/// GitLab-backed ProjectSource
///
/// Fetches files directly from GitLab API without cloning the repository
pub struct GitLabSource {
    client: Arc<reqwest::Client>,
    base_url: String,
    project_id: String, // Can be numeric ID or "namespace/project" format
    reference: String,  // branch, tag, or commit SHA
    access_token: String,
}

impl GitLabSource {
    /// Create a new GitLab source
    ///
    /// # Arguments
    /// * `client` - Reqwest client
    /// * `base_url` - GitLab instance URL (e.g., "https://gitlab.com")
    /// * `project_id` - Project ID or "namespace/project" (URL-encoded)
    /// * `reference` - Branch name, tag, or commit SHA
    /// * `access_token` - GitLab access token (PAT or OAuth)
    pub fn new(
        client: Arc<reqwest::Client>,
        base_url: String,
        project_id: String,
        reference: String,
        access_token: String,
    ) -> Self {
        Self {
            client,
            base_url,
            project_id,
            reference,
            access_token,
        }
    }

    fn location(&self) -> String {
        format!("GitLab project {} at '{}'", self.project_id, self.reference)
    }

    fn transport_error(&self, operation: String, error: reqwest::Error) -> ProjectSourceError {
        ProjectSourceError::Transport {
            location: self.location(),
            operation,
            reason: error.without_url().to_string(),
        }
    }

    fn get_headers(&self) -> ProjectSourceResult<reqwest::header::HeaderMap> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "PRIVATE-TOKEN",
            reqwest::header::HeaderValue::from_str(&self.access_token).map_err(|_| {
                ProjectSourceError::InvalidCredential {
                    location: self.location(),
                    header: "PRIVATE-TOKEN",
                }
            })?,
        );
        Ok(headers)
    }

    fn encode_path(&self, path: &str) -> String {
        urlencoding::encode(path).to_string()
    }
}

#[async_trait]
impl ProjectSource for GitLabSource {
    async fn has_file(&self, path: &str) -> bool {
        debug!(
            "Checking if file exists: {}/{} @ {}",
            self.project_id, path, self.reference
        );

        let encoded_path = self.encode_path(path);
        let url = format!(
            "{}/api/v4/projects/{}/repository/files/{}?ref={}",
            self.base_url,
            urlencoding::encode(&self.project_id),
            encoded_path,
            urlencoding::encode(&self.reference)
        );

        let headers = match self.get_headers() {
            Ok(headers) => headers,
            Err(e) => {
                warn!("Cannot check {}: {}", path, e);
                return false;
            }
        };
        match self.client.head(&url).headers(headers).send().await {
            Ok(response) => response.status().is_success(),
            Err(e) => {
                warn!("Failed to check file existence: {}", e.without_url());
                false
            }
        }
    }

    async fn has_directory(&self, path: &str) -> bool {
        debug!(
            "Checking if directory exists: {}/{} @ {}",
            self.project_id, path, self.reference
        );

        let encoded_path = self.encode_path(path);
        let url = format!(
            "{}/api/v4/projects/{}/repository/tree?path={}&ref={}",
            self.base_url,
            urlencoding::encode(&self.project_id),
            encoded_path,
            urlencoding::encode(&self.reference)
        );

        let headers = match self.get_headers() {
            Ok(headers) => headers,
            Err(e) => {
                warn!("Cannot check {}: {}", path, e);
                return false;
            }
        };
        match self.client.get(&url).headers(headers).send().await {
            Ok(response) => {
                if response.status().is_success() {
                    match response.json::<Vec<GitLabTreeEntry>>().await {
                        Ok(entries) => !entries.is_empty(),
                        Err(_) => false,
                    }
                } else {
                    false
                }
            }
            Err(e) => {
                warn!("Failed to check directory existence: {}", e.without_url());
                false
            }
        }
    }

    async fn read_file(&self, path: &str) -> ProjectSourceResult<String> {
        debug!(
            "Reading file: {}/{} @ {}",
            self.project_id, path, self.reference
        );

        let encoded_path = self.encode_path(path);
        let url = format!(
            "{}/api/v4/projects/{}/repository/files/{}?ref={}",
            self.base_url,
            urlencoding::encode(&self.project_id),
            encoded_path,
            urlencoding::encode(&self.reference)
        );

        let response = self
            .client
            .get(&url)
            .headers(self.get_headers()?)
            .send()
            .await
            .map_err(|e| self.transport_error(format!("fetch '{path}'"), e))?;

        if !response.status().is_success() {
            return Err(ProjectSourceError::from_status(
                self.location(),
                format!("fetch '{path}'"),
                Some(path),
                response.status().as_u16(),
            ));
        }

        let file: GitLabFileContent =
            response
                .json()
                .await
                .map_err(|e| ProjectSourceError::InvalidResponse {
                    location: self.location(),
                    operation: format!("fetch '{path}'"),
                    reason: e.without_url().to_string(),
                })?;

        // GitLab returns base64-encoded content
        if file.encoding == "base64" {
            let decoded = STANDARD
                .decode(file.content.replace('\n', ""))
                .map_err(|e| ProjectSourceError::InvalidContent {
                    location: self.location(),
                    path: path.to_string(),
                    reason: format!("not valid base64: {e}"),
                })?;

            let content_str =
                String::from_utf8(decoded).map_err(|e| ProjectSourceError::InvalidContent {
                    location: self.location(),
                    path: path.to_string(),
                    reason: format!("not valid UTF-8: {e}"),
                })?;

            Ok(content_str.replace("\r\n", "\n"))
        } else {
            // Text encoding (rare)
            Ok(file.content.replace("\r\n", "\n"))
        }
    }

    async fn list_files(&self) -> ProjectSourceResult<Vec<String>> {
        debug!(
            "Listing all files: {} @ {}",
            self.project_id, self.reference
        );

        // Get the tree recursively
        let url = format!(
            "{}/api/v4/projects/{}/repository/tree?recursive=true&ref={}&per_page=100",
            self.base_url,
            urlencoding::encode(&self.project_id),
            urlencoding::encode(&self.reference)
        );

        let mut all_files = Vec::new();
        let mut page = 1;

        loop {
            let paginated_url = format!("{}&page={}", url, page);

            let response = self
                .client
                .get(&paginated_url)
                .headers(self.get_headers()?)
                .send()
                .await
                .map_err(|e| self.transport_error(LIST_FILES.to_string(), e))?;

            if !response.status().is_success() {
                return Err(ProjectSourceError::from_status(
                    self.location(),
                    LIST_FILES.to_string(),
                    None,
                    response.status().as_u16(),
                ));
            }

            let entries: Vec<GitLabTreeEntry> =
                response
                    .json()
                    .await
                    .map_err(|e| ProjectSourceError::InvalidResponse {
                        location: self.location(),
                        operation: LIST_FILES.to_string(),
                        reason: e.without_url().to_string(),
                    })?;

            if entries.is_empty() {
                break;
            }

            // Filter only files (blobs)
            for entry in entries {
                if entry.entry_type == "blob" {
                    all_files.push(entry.path);
                }
            }

            page += 1;

            // Safety: limit to 100 pages (10,000 files max)
            if page > 100 {
                warn!("Reached pagination limit (100 pages)");
                break;
            }
        }

        debug!("Found {} files", all_files.len());
        Ok(all_files)
    }

    async fn find_files(&self, pattern: &str) -> ProjectSourceResult<Vec<String>> {
        debug!(
            "Finding files matching pattern '{}': {} @ {}",
            pattern, self.project_id, self.reference
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

    fn source_over(base_url: String, token: &str) -> GitLabSource {
        GitLabSource::new(
            Arc::new(reqwest::Client::new()),
            base_url,
            "42".to_string(),
            "main".to_string(),
            token.to_string(),
        )
    }

    #[tokio::test]
    async fn statuses_become_typed_errors_without_the_token() {
        let mut server = mockito::Server::new_async().await;
        let _file = server
            .mock(
                "GET",
                "/api/v4/projects/42/repository/files/a.json?ref=main",
            )
            .with_status(401)
            .create_async()
            .await;
        let _tree = server
            .mock(
                "GET",
                "/api/v4/projects/42/repository/tree?recursive=true&ref=main&per_page=100&page=1",
            )
            .with_status(404)
            .create_async()
            .await;
        let source = source_over(server.url(), "SECRETTOKEN");

        let error = source.read_file("a.json").await.expect_err("401");
        assert!(
            matches!(error, ProjectSourceError::AuthFailed { status: 401, .. }),
            "{error:?}"
        );
        assert!(!error.is_retryable());
        assert!(!error.to_string().contains("SECRETTOKEN"));

        let error = source.list_files().await.expect_err("404");
        assert!(
            matches!(&error, ProjectSourceError::NotFound { path, .. } if path == "the repository tree"),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn a_token_that_cannot_be_a_header_is_reported_without_itself() {
        let error = source_over("http://127.0.0.1:9".to_string(), "SECRET\nTOKEN")
            .read_file("a.json")
            .await
            .expect_err("the header cannot be built");
        assert!(
            matches!(
                &error,
                ProjectSourceError::InvalidCredential {
                    header: "PRIVATE-TOKEN",
                    ..
                }
            ),
            "{error:?}"
        );
        assert!(!error.to_string().contains("SECRET"), "{error}");
    }

    #[tokio::test]
    async fn undecodable_content_is_named_and_not_retried() {
        let mut server = mockito::Server::new_async().await;
        let _file = server
            .mock(
                "GET",
                "/api/v4/projects/42/repository/files/a.json?ref=main",
            )
            .with_body(r#"{"file_name":"a.json","content":"***","encoding":"base64"}"#)
            .create_async()
            .await;
        let error = source_over(server.url(), "token")
            .read_file("a.json")
            .await
            .expect_err("bad base64");
        assert!(
            matches!(&error, ProjectSourceError::InvalidContent { path, .. } if path == "a.json"),
            "{error:?}"
        );
        assert!(!error.is_retryable());
    }

    #[test]
    fn test_glob_to_regex() {
        assert_eq!(glob_to_regex("*.json"), r"^[^/]*\.json$");
        assert_eq!(glob_to_regex("**/*.ts"), r"^(?:.*/)?[^/]*\.ts$");
        assert_eq!(glob_to_regex("src/**/*.tsx"), r"^src/(?:.*/)?[^/]*\.tsx$");
        assert_eq!(glob_to_regex("test?.js"), r"^test[^/]\.js$");
    }
}
