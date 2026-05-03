//! Filesystem operations against a standalone sandbox.
//!
//! `read_file` / `write_file` go through the typed `SandboxProvider` trait
//! (which uses native Docker tar streams, avoiding the known phantom-stream
//! hang on silent `cat` execs). `stat` and `mkdir` fall back to shell exec
//! since the trait does not surface them today — we accept a minor
//! latency cost to keep the provider surface small.
//!
//! All paths are validated as absolute to match `@vercel/sandbox` behavior
//! and to prevent callers from smuggling relative paths that traverse
//! into unexpected locations.

use std::collections::HashMap;

use crate::error::SandboxError;
use crate::services::sandbox_service::SandboxService;

/// Output of `stat`. Minimal on purpose — open-agents only checks
/// existence + file vs. directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatInfo {
    pub path: String,
    pub exists: bool,
    pub is_dir: bool,
    pub is_file: bool,
    /// File size in bytes. `0` for directories or non-existent paths.
    pub size: u64,
}

/// One entry in a batched filesystem write.
#[derive(Debug, Clone)]
pub struct BatchWriteEntry {
    pub path: String,
    pub contents: Vec<u8>,
    /// Unix permission bits. `None` → `0o644`.
    pub mode: Option<u32>,
}

impl SandboxService {
    /// Read a file from inside the sandbox.
    pub async fn fs_read(
        &self,
        public_id: &str,
        user_id: i32,
        path: &str,
    ) -> Result<Vec<u8>, SandboxError> {
        validate_absolute(path, "read")?;
        let (_row, internal_id) = self.resolve_id(public_id, user_id).await?;
        self.touch(internal_id).await;
        let handle = self
            .registry()
            .get(internal_id, public_id)
            .await
            .map_err(|e| Self::provider_err(public_id, e))?;
        let bytes = self
            .registry()
            .provider()
            .read_file(&handle, path)
            .await
            .map_err(|e| SandboxError::FileOp {
                sandbox_id: public_id.to_string(),
                op: "read".into(),
                path: path.to_string(),
                reason: e.to_string(),
            })?;
        Ok(bytes)
    }

    /// Write a file into the sandbox, overwriting any existing content.
    /// `mode` is a Unix permission mask; callers that don't care pass
    /// `0o644`.
    pub async fn fs_write(
        &self,
        public_id: &str,
        user_id: i32,
        path: &str,
        contents: &[u8],
        mode: u32,
    ) -> Result<(), SandboxError> {
        validate_absolute(path, "write")?;
        let (_row, internal_id) = self.resolve_id(public_id, user_id).await?;
        self.touch(internal_id).await;
        let handle = self
            .registry()
            .get(internal_id, public_id)
            .await
            .map_err(|e| Self::provider_err(public_id, e))?;
        self.registry()
            .provider()
            .write_file(&handle, path, contents, mode)
            .await
            .map_err(|e| SandboxError::FileOp {
                sandbox_id: public_id.to_string(),
                op: "write".into(),
                path: path.to_string(),
                reason: e.to_string(),
            })?;
        Ok(())
    }

    /// Stat a path inside the sandbox. Returns a populated `StatInfo` with
    /// `exists: false` when the path is missing (not an error).
    pub async fn fs_stat(
        &self,
        public_id: &str,
        user_id: i32,
        path: &str,
    ) -> Result<StatInfo, SandboxError> {
        validate_absolute(path, "stat")?;
        let (_row, internal_id) = self.resolve_id(public_id, user_id).await?;
        self.touch(internal_id).await;
        let handle = self
            .registry()
            .get(internal_id, public_id)
            .await
            .map_err(|e| Self::provider_err(public_id, e))?;

        // `test -e && stat -c '%F %s' <path>` → prints "regular file 42"
        // / "directory 4096" / ... on success, exits non-zero on missing.
        // We parse the first token. Keeps us honest without depending on
        // a specific stat implementation's JSON output.
        let script = format!(
            "if [ -e {p} ]; then if [ -d {p} ]; then echo dir; elif [ -f {p} ]; then wc -c < {p}; else echo other; fi; else echo missing; fi",
            p = shell_quote(path)
        );
        let cmd = vec!["sh".into(), "-c".into(), script];
        let result = self
            .registry()
            .provider()
            .exec(&handle, cmd, HashMap::new(), None)
            .await
            .map_err(|e| SandboxError::FileOp {
                sandbox_id: public_id.to_string(),
                op: "stat".into(),
                path: path.to_string(),
                reason: e.to_string(),
            })?;

        if result.exit_code != 0 {
            return Err(SandboxError::FileOp {
                sandbox_id: public_id.to_string(),
                op: "stat".into(),
                path: path.to_string(),
                reason: format!("stat exited {}: {}", result.exit_code, result.stdout.trim()),
            });
        }

        let out = result.stdout.trim();
        let info = if out == "missing" {
            StatInfo {
                path: path.to_string(),
                exists: false,
                is_dir: false,
                is_file: false,
                size: 0,
            }
        } else if out == "dir" {
            StatInfo {
                path: path.to_string(),
                exists: true,
                is_dir: true,
                is_file: false,
                size: 0,
            }
        } else if out == "other" {
            StatInfo {
                path: path.to_string(),
                exists: true,
                is_dir: false,
                is_file: false,
                size: 0,
            }
        } else {
            // Numeric byte count from `wc -c`.
            let size = out.parse::<u64>().unwrap_or(0);
            StatInfo {
                path: path.to_string(),
                exists: true,
                is_dir: false,
                is_file: true,
                size,
            }
        };
        Ok(info)
    }

    /// Write multiple files in one call. Mirrors `@vercel/sandbox`'s
    /// `writeFiles()`, which is the hot path for seeding project
    /// scaffolding (e.g. a generated Next.js app). Semantics:
    ///
    /// - Fails fast: on the first per-file failure the remaining files
    ///   are skipped and the error is returned. Files already written
    ///   before the failure are left in place (partial-write).
    /// - Empty list is a no-op (not an error).
    /// - Each path must be absolute — validated per file.
    ///
    /// Implementation-wise we resolve the handle once and call the
    /// provider per file, so we avoid N sandbox lookups. A future
    /// optimization could share a single tar upload per common prefix,
    /// but the current provider `write_file` already uses tar streams
    /// under the hood and amortizes well.
    pub async fn fs_write_batch(
        &self,
        public_id: &str,
        user_id: i32,
        files: Vec<BatchWriteEntry>,
    ) -> Result<usize, SandboxError> {
        for entry in &files {
            validate_absolute(&entry.path, "write")?;
        }
        if files.is_empty() {
            return Ok(0);
        }
        let (_row, internal_id) = self.resolve_id(public_id, user_id).await?;
        self.touch(internal_id).await;
        let handle = self
            .registry()
            .get(internal_id, public_id)
            .await
            .map_err(|e| Self::provider_err(public_id, e))?;

        let mut written = 0usize;
        for entry in files {
            let mode = entry.mode.unwrap_or(0o644);
            self.registry()
                .provider()
                .write_file(&handle, &entry.path, &entry.contents, mode)
                .await
                .map_err(|e| SandboxError::FileOp {
                    sandbox_id: public_id.to_string(),
                    op: "write_batch".into(),
                    path: entry.path.clone(),
                    reason: e.to_string(),
                })?;
            written += 1;
        }
        Ok(written)
    }

    /// Create a directory (plus any missing parents). No-ops when the
    /// directory already exists.
    pub async fn fs_mkdir(
        &self,
        public_id: &str,
        user_id: i32,
        path: &str,
    ) -> Result<(), SandboxError> {
        validate_absolute(path, "mkdir")?;
        let (_row, internal_id) = self.resolve_id(public_id, user_id).await?;
        self.touch(internal_id).await;
        let handle = self
            .registry()
            .get(internal_id, public_id)
            .await
            .map_err(|e| Self::provider_err(public_id, e))?;
        let cmd = vec!["mkdir".into(), "-p".into(), path.to_string()];
        let result = self
            .registry()
            .provider()
            .exec(&handle, cmd, HashMap::new(), None)
            .await
            .map_err(|e| SandboxError::FileOp {
                sandbox_id: public_id.to_string(),
                op: "mkdir".into(),
                path: path.to_string(),
                reason: e.to_string(),
            })?;
        if result.exit_code != 0 {
            return Err(SandboxError::FileOp {
                sandbox_id: public_id.to_string(),
                op: "mkdir".into(),
                path: path.to_string(),
                reason: format!(
                    "mkdir exited {}: {}",
                    result.exit_code,
                    result.stdout.trim()
                ),
            });
        }
        Ok(())
    }
}

/// Every public FS op takes an absolute path. Relative paths would be
/// resolved inside the container against an implementation-specific cwd,
/// making behavior brittle.
///
/// Also rejects any `..` (`Component::ParentDir`) component so a request
/// like `/workspace/../etc/passwd` cannot escape the intended cwd. This is
/// the central gate used by every FS handler, including the tar extractor.
fn validate_absolute(path: &str, op: &str) -> Result<(), SandboxError> {
    if path.is_empty() {
        return Err(SandboxError::Validation {
            message: format!("fs {} path must not be empty", op),
        });
    }
    if !path.starts_with('/') {
        return Err(SandboxError::Validation {
            message: format!("fs {} path must be absolute, got '{}'", op, path),
        });
    }
    if std::path::Path::new(path)
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(SandboxError::Validation {
            message: format!(
                "fs {} path must not contain '..' segments, got '{}'",
                op, path
            ),
        });
    }
    Ok(())
}

/// POSIX single-quote escape — see `exec::shell_escape`. Duplicated here
/// so `fs.rs` doesn't depend on exec internals.
fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:@".contains(c))
    {
        s.to_string()
    } else {
        let escaped = s.replace('\'', "'\\''");
        format!("'{}'", escaped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_absolute_rejects_empty() {
        let err = validate_absolute("", "read").unwrap_err();
        assert!(matches!(err, SandboxError::Validation { .. }));
    }

    #[test]
    fn validate_absolute_rejects_relative() {
        let err = validate_absolute("foo/bar", "write").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("absolute"), "msg: {}", msg);
        assert!(msg.contains("foo/bar"), "msg: {}", msg);
    }

    #[test]
    fn validate_absolute_accepts_slash_prefixed() {
        validate_absolute("/workspace/index.ts", "read").unwrap();
    }

    #[test]
    fn validate_absolute_rejects_parent_dir_traversal() {
        let err = validate_absolute("/workspace/../etc/passwd", "write").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains(".."), "msg: {}", msg);
    }

    #[test]
    fn validate_absolute_rejects_trailing_parent_dir() {
        let err = validate_absolute("/workspace/..", "stat").unwrap_err();
        assert!(matches!(err, SandboxError::Validation { .. }));
    }

    #[test]
    fn shell_quote_plain_unchanged() {
        assert_eq!(shell_quote("/tmp/file.txt"), "/tmp/file.txt");
    }

    #[test]
    fn shell_quote_spaces_are_quoted() {
        assert_eq!(shell_quote("/tmp/with space"), "'/tmp/with space'");
    }

    #[test]
    fn stat_info_missing_is_not_exists() {
        let s = StatInfo {
            path: "/x".into(),
            exists: false,
            is_dir: false,
            is_file: false,
            size: 0,
        };
        assert!(!s.exists);
    }
}
