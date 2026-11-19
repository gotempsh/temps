// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Gitea API implementation of ProjectSource
//!
//! Fetches files directly from the Gitea REST API on-demand,
//! enabling framework detection without cloning the repository.

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Deserialize;
use std::sync::Arc;
use temps_presets::source::{ProjectSource, ProjectSourceError, ProjectSourceResult};
use tracing::{debug, warn};

const LIST_FILES: &str = "list the repository tree";

#[derive(Deserialize)]
struct GiteaTreeEntry {
    #[serde(rename = "type")]
    entry_type: String,
    path: String,
}

#[derive(Deserialize)]
struct GiteaFileContent {
    content: Option<String>,
    encoding: Option<String>,
}

/// Gitea-backed `ProjectSource`.
///
/// Fetches files directly from `GET /api/v1/repos/{owner}/{repo}/contents/{path}?ref={ref}`
/// without cloning the repository. Auth: `Authorization: token {pat}`.
pub struct GiteaSource {
    client: Arc<reqwest::Client>,
    base_url: String,
    owner: String,
    repo: String,
    reference: String,
    access_token: String,
}

impl GiteaSource {
    /// Create a new Gitea source.
    ///
    /// # Arguments
    /// * `client` — shared reqwest client
    /// * `base_url` — Gitea instance web root, e.g. `https://git.example.com`
    /// * `owner` — repository owner login
    /// * `repo` — repository name
    /// * `reference` — branch, tag, or commit SHA
    /// * `access_token` — Gitea PAT
    pub fn new(
        client: Arc<reqwest::Client>,
        base_url: String,
        owner: String,
        repo: String,
        reference: String,
        access_token: String,
    ) -> Self {
        Self {
            client,
            base_url,
            owner,
            repo,
            reference,
            access_token,
        }
    }

    fn api_base(&self) -> String {
        format!("{}/api/v1", self.base_url.trim_end_matches('/'))
    }

    fn location(&self) -> String {
        format!(
            "Gitea repository {}/{} at '{}'",
            self.owner, self.repo, self.reference
        )
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
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_str(&format!("token {}", self.access_token))
                .map_err(|_| ProjectSourceError::InvalidCredential {
                    location: self.location(),
                    header: "Authorization",
                })?,
        );
        Ok(headers)
    }

    fn encode_path(&self, path: &str) -> String {
        urlencoding::encode(path).to_string()
    }
}

#[async_trait]
impl ProjectSource for GiteaSource {
    async fn has_file(&self, path: &str) -> bool {
        debug!(
            "Checking if file exists in Gitea {}/{}/{} @ {}",
            self.owner, self.repo, path, self.reference
        );

        let encoded_path = self.encode_path(path);
        let url = format!(
            "{}/repos/{}/{}/contents/{}?ref={}",
            self.api_base(),
            self.owner,
            self.repo,
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
                warn!(
                    "Failed to check Gitea file existence for {}: {}",
                    path,
                    e.without_url()
                );
                false
            }
        }
    }

    async fn has_directory(&self, path: &str) -> bool {
        debug!(
            "Checking if directory exists in Gitea {}/{}/{} @ {}",
            self.owner, self.repo, path, self.reference
        );

        // Gitea returns a list when the contents endpoint targets a directory.
        let encoded_path = self.encode_path(path);
        let url = format!(
            "{}/repos/{}/{}/contents/{}?ref={}",
            self.api_base(),
            self.owner,
            self.repo,
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
                    // If the body is a JSON array, it's a directory listing.
                    match response.json::<Vec<GiteaTreeEntry>>().await {
                        Ok(entries) => !entries.is_empty(),
                        Err(_) => false,
                    }
                } else {
                    false
                }
            }
            Err(e) => {
                warn!(
                    "Failed to check Gitea directory existence for {}: {}",
                    path,
                    e.without_url()
                );
                false
            }
        }
    }

    async fn read_file(&self, path: &str) -> ProjectSourceResult<String> {
        debug!(
            "Reading file from Gitea {}/{}/{} @ {}",
            self.owner, self.repo, path, self.reference
        );

        let encoded_path = self.encode_path(path);
        let url = format!(
            "{}/repos/{}/{}/contents/{}?ref={}",
            self.api_base(),
            self.owner,
            self.repo,
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

        let file: GiteaFileContent =
            response
                .json()
                .await
                .map_err(|e| ProjectSourceError::InvalidResponse {
                    location: self.location(),
                    operation: format!("fetch '{path}'"),
                    reason: e.without_url().to_string(),
                })?;

        let content = file.content.unwrap_or_default();
        let encoding = file.encoding.unwrap_or_default();

        if encoding == "base64" {
            // Gitea (like GitHub) may wrap base64 content in newlines.
            let decoded = STANDARD.decode(content.replace('\n', "")).map_err(|e| {
                ProjectSourceError::InvalidContent {
                    location: self.location(),
                    path: path.to_string(),
                    reason: format!("not valid base64: {e}"),
                }
            })?;

            let content_str =
                String::from_utf8(decoded).map_err(|e| ProjectSourceError::InvalidContent {
                    location: self.location(),
                    path: path.to_string(),
                    reason: format!("not valid UTF-8: {e}"),
                })?;

            Ok(content_str.replace("\r\n", "\n"))
        } else {
            Ok(content.replace("\r\n", "\n"))
        }
    }

    async fn list_files(&self) -> ProjectSourceResult<Vec<String>> {
        debug!(
            "Listing files in Gitea {}/{} @ {}",
            self.owner, self.repo, self.reference
        );

        // Gitea does not have a recursive tree endpoint by default; we use the
        // Git trees API to walk the tree.
        let url = format!(
            "{}/repos/{}/{}/git/trees/{}?recursive=true",
            self.api_base(),
            self.owner,
            self.repo,
            urlencoding::encode(&self.reference)
        );

        let response = self
            .client
            .get(&url)
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

        #[derive(Deserialize)]
        struct GiteaTreeResponse {
            tree: Vec<GiteaTreeEntry>,
        }

        let tree_resp: GiteaTreeResponse =
            response
                .json()
                .await
                .map_err(|e| ProjectSourceError::InvalidResponse {
                    location: self.location(),
                    operation: LIST_FILES.to_string(),
                    reason: e.without_url().to_string(),
                })?;

        let files: Vec<String> = tree_resp
            .tree
            .into_iter()
            .filter(|e| e.entry_type == "blob")
            .map(|e| e.path)
            .collect();

        debug!("Found {} files in Gitea repository", files.len());
        Ok(files)
    }

    async fn find_files(&self, pattern: &str) -> ProjectSourceResult<Vec<String>> {
        debug!(
            "Finding files matching pattern '{}' in Gitea {}/{} @ {}",
            pattern, self.owner, self.repo, self.reference
        );

        let all_files = self.list_files().await?;

        let regex_pattern = glob_to_regex(pattern);
        let re = regex::Regex::new(&regex_pattern)
            .map_err(|e| ProjectSourceError::invalid_pattern(pattern, e))?;

        let matched: Vec<String> = all_files.into_iter().filter(|f| re.is_match(f)).collect();

        debug!(
            "Found {} files matching pattern '{}'",
            matched.len(),
            pattern
        );
        Ok(matched)
    }
}

/// Convert a glob pattern to a regex string (shared with the GitLab source).
fn glob_to_regex(pattern: &str) -> String {
    let mut regex = String::from("^");
    let mut chars = pattern.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '*' if chars.peek() == Some(&'*') => {
                chars.next();
                if chars.peek() == Some(&'/') {
                    chars.next();
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

    fn source_over(base_url: String, token: &str) -> GiteaSource {
        GiteaSource::new(
            Arc::new(reqwest::Client::new()),
            base_url,
            "owner".to_string(),
            "repo".to_string(),
            "main".to_string(),
            token.to_string(),
        )
    }

    #[tokio::test]
    async fn statuses_become_typed_errors_without_the_token() {
        let mut server = mockito::Server::new_async().await;
        for (status, url) in [(404, "/api/v1/repos/owner/repo/contents/a.json?ref=main")] {
            let _mock = server
                .mock("GET", url)
                .with_status(status)
                .create_async()
                .await;
            let error = source_over(server.url(), "SECRETTOKEN")
                .read_file("a.json")
                .await
                .expect_err("the file is missing");
            assert!(
                matches!(&error, ProjectSourceError::NotFound { path, .. } if path == "a.json"),
                "{error:?}"
            );
            assert!(!error.to_string().contains("SECRETTOKEN"));
        }

        let _mock = server
            .mock(
                "GET",
                "/api/v1/repos/owner/repo/git/trees/main?recursive=true",
            )
            .with_status(429)
            .create_async()
            .await;
        let error = source_over(server.url(), "SECRETTOKEN")
            .list_files()
            .await
            .expect_err("the tree is rate limited");
        assert!(
            matches!(error, ProjectSourceError::RateLimited { status: 429, .. }),
            "{error:?}"
        );
        assert!(error.is_retryable());
    }

    #[tokio::test]
    async fn undecodable_content_and_bad_json_are_told_apart() {
        let mut server = mockito::Server::new_async().await;
        let _not_base64 = server
            .mock("GET", "/api/v1/repos/owner/repo/contents/a.json?ref=main")
            .with_body(r#"{"content":"***not base64***","encoding":"base64"}"#)
            .create_async()
            .await;
        let _not_json = server
            .mock("GET", "/api/v1/repos/owner/repo/contents/b.json?ref=main")
            .with_body("<html>")
            .create_async()
            .await;
        let source = source_over(server.url(), "token");

        let error = source.read_file("a.json").await.expect_err("bad base64");
        assert!(
            matches!(error, ProjectSourceError::InvalidContent { .. }),
            "{error:?}"
        );
        let error = source.read_file("b.json").await.expect_err("bad JSON");
        assert!(
            matches!(error, ProjectSourceError::InvalidResponse { .. }),
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
                    header: "Authorization",
                    ..
                }
            ),
            "{error:?}"
        );
        assert!(!error.to_string().contains("SECRET"), "{error}");
    }

    #[tokio::test]
    async fn a_refused_connection_is_retryable_and_hides_the_url() {
        let error = source_over("http://user:hunter2@127.0.0.1:9".to_string(), "token")
            .read_file("a.json")
            .await
            .expect_err("nothing listens on the discard port");
        assert!(
            matches!(error, ProjectSourceError::Transport { .. }),
            "{error:?}"
        );
        assert!(error.is_retryable());
        assert!(!error.to_string().contains("hunter2"), "{error}");
    }

    #[test]
    fn test_glob_to_regex_star() {
        assert_eq!(glob_to_regex("*.json"), r"^[^/]*\.json$");
    }

    #[test]
    fn test_glob_to_regex_double_star() {
        assert_eq!(glob_to_regex("**/*.ts"), r"^(?:.*/)?[^/]*\.ts$");
    }

    #[test]
    fn test_glob_to_regex_nested() {
        assert_eq!(glob_to_regex("src/**/*.tsx"), r"^src/(?:.*/)?[^/]*\.tsx$");
    }

    #[test]
    fn test_api_base_strips_trailing_slash() {
        let source = GiteaSource::new(
            Arc::new(reqwest::Client::new()),
            "https://git.example.com/".to_string(),
            "owner".to_string(),
            "repo".to_string(),
            "main".to_string(),
            "token".to_string(),
        );
        assert_eq!(source.api_base(), "https://git.example.com/api/v1");
    }
}
