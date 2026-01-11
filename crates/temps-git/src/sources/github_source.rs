//! GitHub API implementation of ProjectSource
//!
//! This implementation fetches files directly from GitHub API on-demand

use anyhow::Result;
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use http_body_util::BodyExt;
use octocrab::Octocrab;
use std::sync::Arc;
use temps_presets::source::ProjectSource;
use tracing::debug;

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

    async fn read_file(&self, path: &str) -> Result<String> {
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
            .map_err(|e| anyhow::anyhow!("Failed to fetch file from GitHub: {}", e))?;

        // Get the first item (file content)
        let file = content
            .items
            .first()
            .ok_or_else(|| anyhow::anyhow!("File not found: {}", path))?;

        // Decode base64 content
        if let Some(encoded_content) = &file.content {
            let decoded = STANDARD
                .decode(encoded_content.replace('\n', ""))
                .map_err(|e| anyhow::anyhow!("Failed to decode base64 content: {}", e))?;

            let content_str = String::from_utf8(decoded)
                .map_err(|e| anyhow::anyhow!("File content is not valid UTF-8: {}", e))?;

            Ok(content_str.replace("\r\n", "\n"))
        } else {
            Err(anyhow::anyhow!("File has no content: {}", path))
        }
    }

    async fn list_files(&self) -> Result<Vec<String>> {
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
            .map_err(|e| anyhow::anyhow!("Failed to fetch tree from GitHub: {}", e))?;

        // Read response body and parse JSON
        let body = response
            .into_body()
            .collect()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read response body: {}", e))?
            .to_bytes();

        let tree_response: TreeResponse = serde_json::from_slice(&body)
            .map_err(|e| anyhow::anyhow!("Failed to parse tree response: {}", e))?;

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

    async fn find_files(&self, pattern: &str) -> Result<Vec<String>> {
        debug!(
            "Finding files matching pattern '{}': {}/{} @ {}",
            pattern, self.owner, self.repo, self.reference
        );

        let all_files = self.list_files().await?;

        // Convert glob pattern to regex
        let regex_pattern = glob_to_regex(pattern);
        let re = regex::Regex::new(&regex_pattern)?;

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

    #[test]
    fn test_glob_to_regex() {
        assert_eq!(glob_to_regex("*.json"), r"^[^/]*\.json$");
        assert_eq!(glob_to_regex("**/*.ts"), r"^(?:.*/)?[^/]*\.ts$");
        assert_eq!(glob_to_regex("src/**/*.tsx"), r"^src/(?:.*/)?[^/]*\.tsx$");
        assert_eq!(glob_to_regex("test?.js"), r"^test[^/]\.js$");
    }
}
