//! Bitbucket Cloud API implementation of ProjectSource
//!
//! Fetches files directly from the Bitbucket REST API v2.0 on-demand via
//! `/2.0/repositories/{workspace}/{repo_slug}/src/{commit}/{path}`.
//! Bitbucket returns file content directly (not base64 encoded).

use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use std::sync::Arc;
use temps_presets::source::ProjectSource;
use tracing::{debug, warn};

use crate::services::git_provider::AuthMethod;

/// Bitbucket Cloud API base URL — always HTTPS, never user-supplied.
const API_BASE: &str = "https://api.bitbucket.org/2.0";

/// Bitbucket-backed `ProjectSource`.
///
/// Fetches files directly from:
/// `GET /2.0/repositories/{workspace}/{repo_slug}/src/{commit}/{path}`
///
/// Bitbucket Cloud returns file content directly (not base64), so no decoding
/// is required. Auth is `Basic x-token-auth:{token}` for RAT/WAT or
/// `Basic {username}:{app_password}` for App Passwords.
pub struct BitbucketSource {
    client: Arc<reqwest::Client>,
    auth_method: AuthMethod,
    workspace: String,
    repo: String,
    reference: String,
}

impl BitbucketSource {
    /// Create a new Bitbucket source.
    ///
    /// # Arguments
    /// * `client` — shared reqwest client
    /// * `auth_method` — `PersonalAccessToken` (RAT/WAT) or `BasicAuth` (App Password)
    /// * `workspace` — Bitbucket workspace slug (owner)
    /// * `repo` — repository slug
    /// * `reference` — branch, tag, or commit SHA
    pub fn new(
        client: Arc<reqwest::Client>,
        auth_method: AuthMethod,
        workspace: String,
        repo: String,
        reference: String,
    ) -> Self {
        Self {
            client,
            auth_method,
            workspace,
            repo,
            reference,
        }
    }

    /// Build an authenticated `reqwest::RequestBuilder`.
    fn apply_auth(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.auth_method {
            AuthMethod::PersonalAccessToken { token } => {
                builder.basic_auth("x-token-auth", Some(token))
            }
            AuthMethod::BasicAuth { username, password } => {
                builder.basic_auth(username, Some(password))
            }
            _ => builder,
        }
    }

    /// Construct the src URL for a file path.
    fn src_url(&self, path: &str) -> String {
        format!(
            "{}/repositories/{}/{}/src/{}/{}",
            API_BASE,
            self.workspace,
            self.repo,
            urlencoding::encode(&self.reference),
            path
        )
    }
}

/// Tree entry from Bitbucket's directory listing.
#[derive(Deserialize)]
struct BitbucketTreeEntry {
    #[serde(rename = "type")]
    entry_type: String,
    path: String,
}

/// Paginated response from Bitbucket directory listing.
#[derive(Deserialize)]
struct BitbucketDirPage {
    values: Vec<BitbucketTreeEntry>,
    next: Option<String>,
}

#[async_trait]
impl ProjectSource for BitbucketSource {
    async fn has_file(&self, path: &str) -> bool {
        debug!(
            "Checking if file exists in Bitbucket {}/{}/{} @ {}",
            self.workspace, self.repo, path, self.reference
        );

        let url = self.src_url(path);

        match self.apply_auth(self.client.head(&url)).send().await {
            Ok(response) => response.status().is_success(),
            Err(e) => {
                warn!(
                    "Failed to check Bitbucket file existence for {}: {}",
                    path, e
                );
                false
            }
        }
    }

    async fn has_directory(&self, path: &str) -> bool {
        debug!(
            "Checking if directory exists in Bitbucket {}/{}/{} @ {}",
            self.workspace, self.repo, path, self.reference
        );

        // Bitbucket returns a JSON listing when the path is a directory.
        let url = self.src_url(path);

        match self.apply_auth(self.client.get(&url)).send().await {
            Ok(response) => {
                if !response.status().is_success() {
                    return false;
                }
                // A JSON body with `values` indicates a directory listing.
                match response.json::<BitbucketDirPage>().await {
                    Ok(page) => !page.values.is_empty(),
                    Err(_) => {
                        // Non-JSON response means it's a file, not a directory.
                        false
                    }
                }
            }
            Err(e) => {
                warn!(
                    "Failed to check Bitbucket directory existence for {}: {}",
                    path, e
                );
                false
            }
        }
    }

    async fn read_file(&self, path: &str) -> Result<String> {
        debug!(
            "Reading file from Bitbucket {}/{}/{} @ {}",
            self.workspace, self.repo, path, self.reference
        );

        let url = self.src_url(path);

        let response = self
            .apply_auth(self.client.get(&url))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to fetch file from Bitbucket: {}", e))?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Bitbucket API error: {} - File: {}",
                response.status(),
                path
            ));
        }

        // Bitbucket returns content as plain text (not base64).
        let content = response
            .text()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read Bitbucket file content: {}", e))?;

        Ok(content.replace("\r\n", "\n"))
    }

    async fn list_files(&self) -> Result<Vec<String>> {
        debug!(
            "Listing files in Bitbucket {}/{} @ {}",
            self.workspace, self.repo, self.reference
        );

        // Bitbucket's src endpoint supports recursive listing via `?max_depth=N`.
        // We use a large max_depth to get all files in a single request where possible.
        let url = format!(
            "{}/repositories/{}/{}/src/{}/?max_depth=100&pagelen=100",
            API_BASE,
            self.workspace,
            self.repo,
            urlencoding::encode(&self.reference)
        );

        let mut all_files: Vec<String> = Vec::new();
        let mut next_url: Option<String> = Some(url);

        while let Some(current_url) = next_url {
            let response = self
                .apply_auth(self.client.get(&current_url))
                .send()
                .await
                .map_err(|e| {
                    anyhow::anyhow!("Failed to fetch file listing from Bitbucket: {}", e)
                })?;

            if !response.status().is_success() {
                return Err(anyhow::anyhow!(
                    "Bitbucket API error listing files: {}",
                    response.status()
                ));
            }

            let page: BitbucketDirPage = response
                .json()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to parse Bitbucket file listing: {}", e))?;

            for entry in page.values {
                if entry.entry_type == "commit_file" {
                    all_files.push(entry.path);
                }
            }

            next_url = page.next;
        }

        debug!("Found {} files in Bitbucket repository", all_files.len());
        Ok(all_files)
    }

    async fn find_files(&self, pattern: &str) -> Result<Vec<String>> {
        debug!(
            "Finding files matching pattern '{}' in Bitbucket {}/{} @ {}",
            pattern, self.workspace, self.repo, self.reference
        );

        let all_files = self.list_files().await?;

        let regex_pattern = glob_to_regex(pattern);
        let re = regex::Regex::new(&regex_pattern)?;

        let matched: Vec<String> = all_files.into_iter().filter(|f| re.is_match(f)).collect();

        debug!(
            "Found {} files matching pattern '{}'",
            matched.len(),
            pattern
        );
        Ok(matched)
    }
}

/// Convert a glob pattern to a regex string (mirrors gitea_source.rs).
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

    fn make_source(reference: &str) -> BitbucketSource {
        BitbucketSource::new(
            Arc::new(reqwest::Client::new()),
            AuthMethod::PersonalAccessToken {
                token: "test-token".to_string(),
            },
            "myworkspace".to_string(),
            "myrepo".to_string(),
            reference.to_string(),
        )
    }

    #[test]
    fn test_src_url_construction() {
        let source = make_source("main");
        let url = source.src_url("src/main.rs");
        assert_eq!(
            url,
            "https://api.bitbucket.org/2.0/repositories/myworkspace/myrepo/src/main/src/main.rs"
        );
    }

    #[test]
    fn test_src_url_encodes_branch_with_slash() {
        let source = make_source("feature/my-branch");
        let url = source.src_url("README.md");
        assert!(
            url.contains("feature%2Fmy-branch"),
            "branch with slash must be URL-encoded"
        );
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
}
