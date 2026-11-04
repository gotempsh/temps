// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Source abstraction for accessing project files
//!
//! This module provides traits for accessing project files from different sources:
//! - Local filesystem
//! - GitHub API (via git provider tree API)
//! - Any other remote source
//!
//! Presets use these traits to detect frameworks and read configuration files
//! without needing to know where the files come from.

use async_trait::async_trait;
use std::collections::HashMap;

/// Why a [`ProjectSource`] could not answer.
///
/// `location` names the source (provider, repository and revision) and never
/// its credentials or the URL it talks to.
#[derive(Debug, thiserror::Error)]
pub enum ProjectSourceError {
    #[error("'{path}' was not found in {location}")]
    NotFound { location: String, path: String },

    #[error("{location} refused the credentials while trying to {operation} (status {status})")]
    AuthFailed {
        location: String,
        operation: String,
        status: u16,
    },

    #[error("{location} rate limited the request to {operation} (status {status})")]
    RateLimited {
        location: String,
        operation: String,
        status: u16,
    },

    #[error("{location} answered status {status} while trying to {operation}")]
    Upstream {
        location: String,
        operation: String,
        status: u16,
    },

    #[error("The request to {operation} on {location} failed: {reason}")]
    Transport {
        location: String,
        operation: String,
        reason: String,
    },

    #[error("Could not parse the answer to {operation} from {location}: {reason}")]
    InvalidResponse {
        location: String,
        operation: String,
        reason: String,
    },

    #[error("'{path}' in {location} could not be decoded: {reason}")]
    InvalidContent {
        location: String,
        path: String,
        reason: String,
    },

    #[error("'{path}' in {location} has no file content")]
    MissingContent { location: String, path: String },

    #[error(
        "The access token for {location} cannot be sent in the {header} header: it contains \
         characters that are not valid in an HTTP header value"
    )]
    InvalidCredential {
        location: String,
        header: &'static str,
    },

    #[error("Invalid file pattern '{pattern}': {reason}")]
    InvalidPattern { pattern: String, reason: String },

    #[error("Failed to {operation} '{}': {source}", .path.display())]
    Io {
        operation: &'static str,
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub type ProjectSourceResult<T> = std::result::Result<T, ProjectSourceError>;

impl ProjectSourceError {
    /// Classify a non-success HTTP status. `path` is the file asked for; a 404
    /// without one means the repository or revision itself is missing.
    pub fn from_status(
        location: String,
        operation: String,
        path: Option<&str>,
        status: u16,
    ) -> Self {
        match status {
            404 => Self::NotFound {
                location,
                path: path.unwrap_or("the repository tree").to_string(),
            },
            401 | 403 => Self::AuthFailed {
                location,
                operation,
                status,
            },
            429 => Self::RateLimited {
                location,
                operation,
                status,
            },
            _ => Self::Upstream {
                location,
                operation,
                status,
            },
        }
    }

    /// Build the error for a glob that does not compile to a regex.
    pub fn invalid_pattern(pattern: &str, error: regex::Error) -> Self {
        Self::InvalidPattern {
            pattern: pattern.to_string(),
            reason: error.to_string(),
        }
    }

    /// Whether the same request may succeed if sent again later.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::RateLimited { .. } | Self::Transport { .. } => true,
            Self::Upstream { status, .. } => *status >= 500,
            Self::NotFound { .. }
            | Self::AuthFailed { .. }
            | Self::InvalidResponse { .. }
            | Self::InvalidContent { .. }
            | Self::MissingContent { .. }
            | Self::InvalidCredential { .. }
            | Self::InvalidPattern { .. }
            | Self::Io { .. } => false,
        }
    }
}

/// Trait for accessing project file tree and contents
///
/// Implementations can fetch from:
/// - Local filesystem
/// - GitHub/GitLab API
/// - S3/Object storage
/// - Any other source
#[async_trait]
pub trait ProjectSource: Send + Sync {
    /// Check if a file exists in the project
    ///
    /// # Arguments
    /// * `path` - Relative path from project root (e.g., "package.json", "src/index.ts")
    ///
    /// # Returns
    /// `true` if the file exists, `false` otherwise
    async fn has_file(&self, path: &str) -> bool;

    /// Check if a directory exists in the project
    ///
    /// # Arguments
    /// * `path` - Relative path from project root (e.g., "src", "node_modules")
    ///
    /// # Returns
    /// `true` if the directory exists, `false` otherwise
    async fn has_directory(&self, path: &str) -> bool;

    /// Read file contents
    ///
    /// # Arguments
    /// * `path` - Relative path from project root
    ///
    /// # Returns
    /// File contents as a string, or error if file doesn't exist or can't be read
    async fn read_file(&self, path: &str) -> ProjectSourceResult<String>;

    /// Get list of all files in the project
    ///
    /// # Returns
    /// Vector of relative file paths from project root
    async fn list_files(&self) -> ProjectSourceResult<Vec<String>>;

    /// Find files matching a pattern (supports glob-like patterns)
    ///
    /// # Arguments
    /// * `pattern` - Pattern like "*.json", "**/*.ts", "src/**/*.tsx"
    ///
    /// # Returns
    /// Vector of matching file paths
    async fn find_files(&self, pattern: &str) -> ProjectSourceResult<Vec<String>>;
}

/// In-memory implementation of ProjectSource
///
/// Used when you already have the file tree (e.g., from GitHub API)
pub struct InMemorySource {
    files: HashMap<String, String>,
    paths: Vec<String>,
}

impl InMemorySource {
    /// Create from a map of file paths to contents
    ///
    /// # Example
    /// ```
    /// use std::collections::HashMap;
    /// use temps_presets::source::InMemorySource;
    ///
    /// let mut files = HashMap::new();
    /// files.insert("package.json".to_string(), r#"{"name":"test"}"#.to_string());
    /// files.insert("src/index.ts".to_string(), "console.log('hello')".to_string());
    ///
    /// let source = InMemorySource::from_files(files);
    /// ```
    pub fn from_files(files: HashMap<String, String>) -> Self {
        let paths = files.keys().cloned().collect();
        Self { files, paths }
    }

    /// Create from GitHub tree API response
    ///
    /// # Arguments
    /// * `tree` - File tree from GitHub API (path -> content mapping)
    pub fn from_git_tree(tree: HashMap<String, String>) -> Self {
        Self::from_files(tree)
    }
}

#[async_trait]
impl ProjectSource for InMemorySource {
    async fn has_file(&self, path: &str) -> bool {
        self.files.contains_key(path)
    }

    async fn has_directory(&self, path: &str) -> bool {
        let dir_prefix = format!("{}/", path);
        self.paths
            .iter()
            .any(|p| p.starts_with(&dir_prefix) || p == path)
    }

    async fn read_file(&self, path: &str) -> ProjectSourceResult<String> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| ProjectSourceError::NotFound {
                location: "the in-memory source".to_string(),
                path: path.to_string(),
            })
    }

    async fn list_files(&self) -> ProjectSourceResult<Vec<String>> {
        Ok(self.paths.clone())
    }

    async fn find_files(&self, pattern: &str) -> ProjectSourceResult<Vec<String>> {
        let regex_pattern = glob_to_regex(pattern);
        let re = regex::Regex::new(&regex_pattern)
            .map_err(|e| ProjectSourceError::invalid_pattern(pattern, e))?;

        Ok(self
            .paths
            .iter()
            .filter(|p| re.is_match(p))
            .cloned()
            .collect())
    }
}

/// Filesystem implementation of ProjectSource
///
/// Reads files directly from the local filesystem
pub struct FilesystemSource {
    root_path: std::path::PathBuf,
}

impl FilesystemSource {
    /// Create a new filesystem source
    ///
    /// # Arguments
    /// * `root_path` - Absolute path to project root directory
    pub fn new(root_path: std::path::PathBuf) -> Self {
        Self { root_path }
    }
}

#[async_trait]
impl ProjectSource for FilesystemSource {
    async fn has_file(&self, path: &str) -> bool {
        let full_path = self.root_path.join(path);
        full_path.exists() && full_path.is_file()
    }

    async fn has_directory(&self, path: &str) -> bool {
        let full_path = self.root_path.join(path);
        full_path.exists() && full_path.is_dir()
    }

    async fn read_file(&self, path: &str) -> ProjectSourceResult<String> {
        let full_path = self.root_path.join(path);
        let contents = tokio::fs::read_to_string(&full_path)
            .await
            .map_err(|source| match source.kind() {
                std::io::ErrorKind::NotFound => ProjectSourceError::NotFound {
                    location: self.root_path.display().to_string(),
                    path: path.to_string(),
                },
                _ => ProjectSourceError::Io {
                    operation: "read",
                    path: full_path.clone(),
                    source,
                },
            })?;
        Ok(contents.replace("\r\n", "\n"))
    }

    async fn list_files(&self) -> ProjectSourceResult<Vec<String>> {
        let mut files = Vec::new();
        Self::walk_dir(&self.root_path, &self.root_path, &mut files).await?;
        Ok(files)
    }

    async fn find_files(&self, pattern: &str) -> ProjectSourceResult<Vec<String>> {
        let files = self.list_files().await?;
        let regex_pattern = glob_to_regex(pattern);
        let re = regex::Regex::new(&regex_pattern)
            .map_err(|e| ProjectSourceError::invalid_pattern(pattern, e))?;

        Ok(files.into_iter().filter(|p| re.is_match(p)).collect())
    }
}

impl FilesystemSource {
    fn walk_dir<'a>(
        dir: &'a std::path::Path,
        base: &'a std::path::Path,
        files: &'a mut Vec<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ProjectSourceResult<()>> + Send + 'a>>
    {
        Box::pin(async move {
            let list_error = |source| ProjectSourceError::Io {
                operation: "list",
                path: dir.to_path_buf(),
                source,
            };
            let mut entries = tokio::fs::read_dir(dir).await.map_err(list_error)?;

            while let Some(entry) = entries.next_entry().await.map_err(list_error)? {
                // Never follow symlinks: a link out of the tree lists host
                // paths, and a link back into it makes the walk unbounded.
                // `DirEntry::file_type` does not traverse the link.
                let file_type = entry.file_type().await.map_err(list_error)?;
                if file_type.is_symlink() {
                    continue;
                }
                let path = entry.path();

                if let Ok(relative) = path.strip_prefix(base) {
                    let relative_str = relative.to_string_lossy().to_string();

                    if file_type.is_file() {
                        files.push(relative_str);
                    } else if file_type.is_dir() {
                        Self::walk_dir(&path, base, files).await?;
                    }
                }
            }

            Ok(())
        })
    }
}

/// Convert glob pattern to regex
fn glob_to_regex(pattern: &str) -> String {
    let mut regex = String::from("^");
    let mut chars = pattern.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '*' if chars.peek() == Some(&'*') => {
                // ** matches any number of directories
                chars.next(); // consume second *
                if chars.peek() == Some(&'/') {
                    chars.next(); // consume /
                    regex.push_str("(?:.*/)?");
                } else {
                    regex.push_str(".*");
                }
            }
            '*' => {
                // * matches anything except /
                regex.push_str("[^/]*");
            }
            '?' => {
                regex.push_str("[^/]");
            }
            '.' | '(' | ')' | '+' | '|' | '^' | '$' | '@' | '%' => {
                // Escape special regex characters
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

    #[tokio::test]
    async fn test_in_memory_source() {
        let mut files = HashMap::new();
        files.insert("package.json".to_string(), r#"{"name":"test"}"#.to_string());
        files.insert("src/index.ts".to_string(), "console.log('hello')".to_string());

        let source = InMemorySource::from_files(files);

        assert!(source.has_file("package.json").await);
        assert!(source.has_file("src/index.ts").await);
        assert!(!source.has_file("nonexistent.txt").await);

        assert!(source.has_directory("src").await);
        assert!(!source.has_directory("dist").await);

        let content = source.read_file("package.json").await.unwrap();
        assert!(content.contains("test"));

        let files = source.list_files().await.unwrap();
        assert_eq!(files.len(), 2);
    }

    #[tokio::test]
    async fn test_in_memory_find_files() {
        let mut files = HashMap::new();
        files.insert("package.json".to_string(), "{}".to_string());
        files.insert("src/index.ts".to_string(), "".to_string());
        files.insert("src/utils/helper.ts".to_string(), "".to_string());
        files.insert("README.md".to_string(), "".to_string());

        let source = InMemorySource::from_files(files);

        let ts_files = source.find_files("**/*.ts").await.unwrap();
        assert_eq!(ts_files.len(), 2);

        let json_files = source.find_files("*.json").await.unwrap();
        assert_eq!(json_files.len(), 1);

        let src_files = source.find_files("src/**/*").await.unwrap();
        assert_eq!(src_files.len(), 2);
    }

    #[tokio::test]
    async fn test_from_git_tree() {
        let mut tree = HashMap::new();
        tree.insert("package.json".to_string(), r#"{"name":"app"}"#.to_string());
        tree.insert("next.config.js".to_string(), "module.exports = {}".to_string());

        let source = InMemorySource::from_git_tree(tree);

        assert!(source.has_file("package.json").await);
        assert!(source.has_file("next.config.js").await);

        let content = source.read_file("package.json").await.unwrap();
        assert!(content.contains("app"));
    }

    #[tokio::test]
    async fn a_missing_in_memory_file_is_not_found() {
        let source = InMemorySource::from_files(HashMap::new());
        let error = source.read_file("nope.json").await.unwrap_err();
        assert!(
            matches!(&error, ProjectSourceError::NotFound { path, .. } if path == "nope.json")
        );
        assert_eq!(
            error.to_string(),
            "'nope.json' was not found in the in-memory source"
        );
    }

    #[tokio::test]
    async fn filesystem_errors_separate_missing_files_from_io_failures() {
        let root = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(root.path().join("a_directory")).unwrap();
        let source = FilesystemSource::new(root.path().to_path_buf());

        assert!(matches!(
            source.read_file("missing.txt").await,
            Err(ProjectSourceError::NotFound { .. })
        ));
        // Reading a directory is an I/O failure, not a missing file.
        let error = source.read_file("a_directory").await.unwrap_err();
        assert!(
            matches!(&error, ProjectSourceError::Io { operation: "read", .. }),
            "{error:?}"
        );

        let missing_root = FilesystemSource::new(root.path().join("no_such_root"));
        let error = missing_root.list_files().await.unwrap_err();
        assert!(
            matches!(&error, ProjectSourceError::Io { operation: "list", .. }),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn an_uncompilable_pattern_is_named() {
        let source = InMemorySource::from_files(HashMap::new());
        let error = source.find_files("[").await.unwrap_err();
        assert!(
            matches!(&error, ProjectSourceError::InvalidPattern { pattern, .. } if pattern == "["),
            "{error:?}"
        );
    }

    #[test]
    fn statuses_classify_into_what_a_caller_acts_on() {
        let classify = |path, status| {
            ProjectSourceError::from_status(
                "GitHub repository o/r at 'main'".to_string(),
                "fetch 'a'".to_string(),
                path,
                status,
            )
        };

        let missing = classify(Some("a"), 404);
        assert!(matches!(&missing, ProjectSourceError::NotFound { path, .. } if path == "a"));
        assert!(!missing.is_retryable());
        assert!(matches!(
            classify(None, 404),
            ProjectSourceError::NotFound { path, .. } if path == "the repository tree"
        ));

        for status in [401, 403] {
            let refused = classify(Some("a"), status);
            assert!(matches!(refused, ProjectSourceError::AuthFailed { .. }));
            assert!(!refused.is_retryable());
        }

        let limited = classify(Some("a"), 429);
        assert!(matches!(limited, ProjectSourceError::RateLimited { .. }));
        assert!(limited.is_retryable());

        let outage = classify(Some("a"), 503);
        assert!(matches!(outage, ProjectSourceError::Upstream { .. }));
        assert!(outage.is_retryable());

        let rejected = classify(Some("a"), 422);
        assert!(matches!(rejected, ProjectSourceError::Upstream { .. }));
        assert!(!rejected.is_retryable());
    }

    #[test]
    fn test_glob_to_regex() {
        assert_eq!(glob_to_regex("*.json"), r"^[^/]*\.json$");
        assert_eq!(glob_to_regex("**/*.ts"), r"^(?:.*/)?[^/]*\.ts$");
        assert_eq!(glob_to_regex("src/**/*.tsx"), r"^src/(?:.*/)?[^/]*\.tsx$");
        assert_eq!(glob_to_regex("test?.js"), r"^test[^/]\.js$");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_filesystem_source_does_not_follow_symlinks() {
        let root = tempfile::TempDir::new().unwrap();
        let outside = tempfile::TempDir::new().unwrap();
        std::fs::write(root.path().join("package.json"), "{}").unwrap();
        std::fs::write(outside.path().join("secret.txt"), "SECRET").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("linked_dir")).unwrap();
        // A file link is a separate case: a walker could skip directory links
        // and still list linked files.
        std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            root.path().join("pnpm-workspace.yaml"),
        )
        .unwrap();
        std::os::unix::fs::symlink(".", root.path().join("a")).unwrap();
        std::os::unix::fs::symlink(".", root.path().join("b")).unwrap();

        let source = FilesystemSource::new(root.path().to_path_buf());
        let files = tokio::time::timeout(std::time::Duration::from_secs(10), source.list_files())
            .await
            .expect("list_files did not finish within 10s: the walk followed a symlink loop")
            .unwrap();

        assert_eq!(files, vec!["package.json".to_string()]);
    }
}
