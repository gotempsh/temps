// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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

/// Most entries one directory listing returns. A directory with more is
/// reported as `truncated` rather than streamed without bound: the listing is
/// held in memory on the control plane and rendered by a browser.
pub const MAX_LIST_ENTRIES: usize = 1000;

/// What a directory entry is, without following a symlink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirEntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

/// One name in a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub kind: DirEntryKind,
}

/// Output of `list_dir`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirListing {
    pub path: String,
    pub exists: bool,
    pub is_dir: bool,
    /// Directories first, then names in byte order. At most
    /// [`MAX_LIST_ENTRIES`].
    pub entries: Vec<DirEntry>,
    /// The directory holds more than [`MAX_LIST_ENTRIES`] entries.
    pub truncated: bool,
}

/// One entry in a batched filesystem write.
#[derive(Debug, Clone)]
pub struct BatchWriteEntry {
    pub path: String,
    pub contents: Vec<u8>,
    /// Unix permission bits. `None` → `0o644`.
    pub mode: Option<u32>,
}

/// Map a provider error from a filesystem operation. Errors that already
/// carry a precise meaning keep it — a rejected request (400, e.g. a file
/// over a worker node's read limit), a feature a worker node can't serve
/// (422), an unreachable node (503), a missing container (404) — and
/// everything else is a failed file operation naming the path.
fn file_op_err(
    public_id: &str,
    op: &str,
    path: impl Into<String>,
    err: temps_agents::error::AgentError,
) -> SandboxError {
    use temps_agents::error::AgentError;
    match err {
        AgentError::Validation { .. }
        | AgentError::SandboxUnsupportedOnNode { .. }
        | AgentError::SandboxNodeUnavailable { .. }
        | AgentError::SandboxProviderUnavailable { .. }
        | AgentError::SandboxNotFound { .. } => crate::error::from_agent_error(public_id, err),
        other => SandboxError::FileOp {
            sandbox_id: public_id.to_string(),
            op: op.to_string(),
            path: path.into(),
            reason: other.to_string(),
        },
    }
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
        let (row, internal_id) = self.resolve_id(public_id, user_id).await?;
        self.touch(internal_id, row.timeout_secs).await;
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
            .map_err(|e| file_op_err(public_id, "read", path.to_string(), e))?;
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
        let (row, internal_id) = self.resolve_id(public_id, user_id).await?;
        self.touch(internal_id, row.timeout_secs).await;
        let handle = self
            .registry()
            .get(internal_id, public_id)
            .await
            .map_err(|e| Self::provider_err(public_id, e))?;
        self.registry()
            .provider()
            .write_file(&handle, path, contents, mode)
            .await
            .map_err(|e| file_op_err(public_id, "write", path.to_string(), e))?;
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
        let (row, internal_id) = self.resolve_id(public_id, user_id).await?;
        self.touch(internal_id, row.timeout_secs).await;
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
            .map_err(|e| file_op_err(public_id, "stat", path.to_string(), e))?;

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

    /// List the entries of a directory inside the sandbox.
    ///
    /// Like `stat`, a missing path is not an error (`exists: false`), and a
    /// path that is not a directory reports `is_dir: false` with no entries.
    /// Symlinks are reported as such and never followed, so a link cannot make
    /// the listing wander outside the directory that was asked for.
    pub async fn fs_list(
        &self,
        public_id: &str,
        user_id: i32,
        path: &str,
    ) -> Result<DirListing, SandboxError> {
        validate_absolute(path, "list")?;
        let (row, internal_id) = self.resolve_id(public_id, user_id).await?;
        self.touch(internal_id, row.timeout_secs).await;
        let handle = self
            .registry()
            .get(internal_id, public_id)
            .await
            .map_err(|e| Self::provider_err(public_id, e))?;

        let cmd = vec!["sh".into(), "-c".into(), list_script(path)];
        let result = self
            .registry()
            .provider()
            .exec(&handle, cmd, HashMap::new(), None)
            .await
            .map_err(|e| file_op_err(public_id, "list", path.to_string(), e))?;

        if result.exit_code != 0 {
            return Err(SandboxError::FileOp {
                sandbox_id: public_id.to_string(),
                op: "list".into(),
                path: path.to_string(),
                reason: format!("list exited {}: {}", result.exit_code, result.stdout.trim()),
            });
        }
        Ok(parse_list_output(path, &result.stdout))
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
        let (row, internal_id) = self.resolve_id(public_id, user_id).await?;
        self.touch(internal_id, row.timeout_secs).await;
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
                .map_err(|e| file_op_err(public_id, "write_batch", entry.path.clone(), e))?;
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
        let (row, internal_id) = self.resolve_id(public_id, user_id).await?;
        self.touch(internal_id, row.timeout_secs).await;
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
            .map_err(|e| file_op_err(public_id, "mkdir", path.to_string(), e))?;
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

/// Shell script that prints `missing`, `notdir`, `denied`, or `ok` followed by
/// one `<k> <name>` line per entry (`k` is `f`, `d`, `l` or `o`).
///
/// Written for busybox `sh`: no `find -printf`, no arrays. Names are expanded
/// by the shell's own globbing relative to the directory, so none of them is
/// ever re-parsed as part of a command line. A name containing a newline would
/// break the line framing and is skipped. One entry past the cap is emitted so
/// the caller can tell "exactly the cap" from "more than the cap".
fn list_script(path: &str) -> String {
    format!(
        "d={p}\n\
         if [ ! -e \"$d\" ]; then echo missing; exit 0; fi\n\
         if [ ! -d \"$d\" ]; then echo notdir; exit 0; fi\n\
         cd -- \"$d\" 2>/dev/null || {{ echo denied; exit 0; }}\n\
         echo ok\n\
         n=0\n\
         for f in .[!.]* ..?* *; do\n\
           if [ ! -e \"$f\" ] && [ ! -L \"$f\" ]; then continue; fi\n\
           case \"$f\" in *'\n'*) continue;; esac\n\
           if [ -L \"$f\" ]; then k=l; elif [ -d \"$f\" ]; then k=d; elif [ -f \"$f\" ]; then k=f; else k=o; fi\n\
           printf '%s %s\\n' \"$k\" \"$f\"\n\
           n=$((n+1))\n\
           if [ \"$n\" -gt {cap} ]; then break; fi\n\
         done\n",
        p = shell_quote(path),
        cap = MAX_LIST_ENTRIES,
    )
}

/// Turn the output of [`list_script`] into a [`DirListing`].
fn parse_list_output(path: &str, stdout: &str) -> DirListing {
    let mut lines = stdout.lines();
    let status = lines.next().map(str::trim).unwrap_or("");
    let listing = |exists: bool, is_dir: bool| DirListing {
        path: path.to_string(),
        exists,
        is_dir,
        entries: Vec::new(),
        truncated: false,
    };
    match status {
        "missing" => return listing(false, false),
        // A directory the sandbox user cannot enter: it exists and is a
        // directory, its contents just are not visible.
        "denied" => return listing(true, true),
        "notdir" => return listing(true, false),
        _ => {}
    }

    let mut entries: Vec<DirEntry> = lines
        .filter_map(|line| {
            let (kind, name) = line.split_once(' ')?;
            if name.is_empty() {
                return None;
            }
            let kind = match kind {
                "f" => DirEntryKind::File,
                "d" => DirEntryKind::Directory,
                "l" => DirEntryKind::Symlink,
                "o" => DirEntryKind::Other,
                _ => return None,
            };
            Some(DirEntry {
                name: name.to_string(),
                kind,
            })
        })
        .collect();

    let truncated = entries.len() > MAX_LIST_ENTRIES;
    entries.truncate(MAX_LIST_ENTRIES);
    entries.sort_by(|a, b| {
        let a_dir = a.kind == DirEntryKind::Directory;
        let b_dir = b.kind == DirEntryKind::Directory;
        b_dir.cmp(&a_dir).then_with(|| a.name.cmp(&b.name))
    });
    DirListing {
        entries,
        truncated,
        ..listing(true, true)
    }
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
    use temps_agents::error::AgentError;

    #[test]
    fn file_op_errors_keep_precise_provider_meanings() {
        // A file over a worker node's read limit is the caller's error (400).
        let err = file_op_err(
            "sbx_1",
            "read",
            "/big.bin",
            AgentError::Validation {
                message: "file /big.bin is larger than 104857600 bytes".into(),
            },
        );
        assert!(matches!(err, SandboxError::Validation { .. }), "{err:?}");

        let err = file_op_err(
            "sbx_1",
            "write",
            "/a",
            AgentError::SandboxNodeUnavailable {
                node_id: 3,
                node_name: "worker-1".into(),
                reason: "offline".into(),
            },
        );
        assert!(matches!(err, SandboxError::Unavailable { .. }), "{err:?}");

        let err = file_op_err(
            "sbx_1",
            "read",
            "/a",
            AgentError::SandboxNotFound {
                run_id: 0,
                sandbox: "temps-sandbox-1".into(),
            },
        );
        assert!(matches!(err, SandboxError::NotFound { .. }), "{err:?}");
    }

    #[test]
    fn other_provider_failures_are_file_op_errors_naming_the_path() {
        let err = file_op_err(
            "sbx_1",
            "mkdir",
            "/workspace/x",
            AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: "temps-sandbox-1".into(),
                reason: "boom".into(),
            },
        );
        match err {
            SandboxError::FileOp { op, path, .. } => {
                assert_eq!(op, "mkdir");
                assert_eq!(path, "/workspace/x");
            }
            other => panic!("expected FileOp, got {other:?}"),
        }
    }

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

    #[test]
    fn list_script_quotes_the_path_and_caps_the_listing() {
        let script = list_script("/work space/it's");
        assert!(script.starts_with("d='/work space/it'\\''s'\n"), "{script}");
        assert!(script.contains("-gt 1000"), "{script}");
    }

    #[test]
    fn parse_list_reports_missing_and_not_a_directory() {
        let missing = parse_list_output("/nope", "missing\n");
        assert!(!missing.exists && !missing.is_dir && missing.entries.is_empty());

        let file = parse_list_output("/etc/hosts", "notdir\n");
        assert!(file.exists && !file.is_dir && file.entries.is_empty());

        let denied = parse_list_output("/root", "denied\n");
        assert!(denied.exists && denied.is_dir && denied.entries.is_empty());
    }

    #[test]
    fn parse_list_sorts_directories_first_and_keeps_awkward_names() {
        let listing = parse_list_output(
            "/work",
            "ok\nf zebra.txt\nd src\nl current\nf  leading space\no fifo\nd .git\n",
        );
        let names: Vec<_> = listing.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [
                ".git",
                "src",
                " leading space",
                "current",
                "fifo",
                "zebra.txt"
            ]
        );
        assert_eq!(listing.entries[3].kind, DirEntryKind::Symlink);
        assert!(!listing.truncated);
        assert!(listing.exists && listing.is_dir);
    }

    #[test]
    fn parse_list_ignores_malformed_lines() {
        let listing = parse_list_output("/work", "ok\nx unknown-kind\nnospace\nf \nf real\n");
        assert_eq!(listing.entries.len(), 1);
        assert_eq!(listing.entries[0].name, "real");
    }

    #[test]
    fn parse_list_flags_a_listing_over_the_cap() {
        let mut out = String::from("ok\n");
        for i in 0..=MAX_LIST_ENTRIES {
            out.push_str(&format!("f file-{i:05}\n"));
        }
        let listing = parse_list_output("/big", &out);
        assert!(listing.truncated);
        assert_eq!(listing.entries.len(), MAX_LIST_ENTRIES);

        let exact = parse_list_output(
            "/big",
            &out.lines()
                .take(MAX_LIST_ENTRIES + 1)
                .collect::<Vec<_>>()
                .join("\n"),
        );
        assert!(!exact.truncated, "exactly the cap is not truncated");
    }
}
