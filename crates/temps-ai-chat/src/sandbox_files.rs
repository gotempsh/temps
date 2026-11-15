// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What the chat's working tabs show of its sandbox: one directory at a time,
//! one file, and the Git changes of the repositories under the work directory,
//! with the two sides of a file's diff.
//!
//! Everything runs as a command in the user's own sandbox, with arguments
//! passed as argv (never spliced into a shell line), and every result is
//! bounded: a listing stops at [`MAX_LIST_ENTRIES`], a file at
//! [`MAX_VIEW_BYTES`], the changes at [`MAX_REPOS`] repositories of
//! [`MAX_CHANGED_FILES`] files each.

use std::collections::HashMap;
use std::time::Duration;

use serde::Serialize;
use temps_entities::ai_conversations;
use temps_sandbox::services::ExecOptions;
use thiserror::Error;
use utoipa::ToSchema;

use crate::sandbox_link::{resolve_path, SandboxLinkError, SandboxLinkService};

/// Most entries one directory listing returns.
pub const MAX_LIST_ENTRIES: usize = 2000;
/// Most bytes of one file (or one side of a diff) the viewer gets.
pub const MAX_VIEW_BYTES: usize = 1024 * 1024;
/// Most repositories the changes view looks at.
pub const MAX_REPOS: usize = 10;
/// Most changed files reported per repository.
pub const MAX_CHANGED_FILES: usize = 500;
/// How deep under the work directory repositories are looked for.
const REPO_SEARCH_DEPTH: usize = 3;
/// How long one command may take before the request gives up on it.
const COMMAND_TIMEOUT_SECS: u64 = 30;
/// Bytes inspected to tell a binary file from text.
const BINARY_SNIFF_BYTES: usize = 8000;
/// Separates one repository's section from the next in the changes script.
const REPO_MARKER: char = '\u{1e}';

#[derive(Error, Debug)]
pub enum SandboxFilesError {
    #[error(transparent)]
    Link(#[from] SandboxLinkError),

    #[error("The sandbox of conversation {conversation_id} is not ready: {reason}")]
    NotReady {
        conversation_id: String,
        reason: String,
    },

    #[error("Invalid path '{path}' for the sandbox of conversation {conversation_id}: {reason}")]
    InvalidPath {
        conversation_id: String,
        path: String,
        reason: String,
    },

    #[error("'{path}' does not exist in the sandbox of conversation {conversation_id}")]
    NotFound {
        conversation_id: String,
        path: String,
    },

    #[error("'{path}' in the sandbox of conversation {conversation_id} is already inside a Git repository, so its changes are already tracked")]
    AlreadyTracked {
        conversation_id: String,
        path: String,
    },

    #[error("Could not {operation} in the sandbox of conversation {conversation_id}: {reason}")]
    Command {
        conversation_id: String,
        operation: String,
        reason: String,
    },

    #[error("Timed out after {secs}s trying to {operation} in the sandbox of conversation {conversation_id}")]
    Timeout {
        conversation_id: String,
        operation: String,
        secs: u64,
    },
}

/// What a directory entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SandboxEntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

/// One entry of a sandbox directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct SandboxEntry {
    pub name: String,
    /// Absolute path inside the sandbox.
    pub path: String,
    pub kind: SandboxEntryKind,
    /// Size in bytes as the filesystem reports it.
    pub size: u64,
}

/// One directory of the chat's sandbox: folders first, then files, by name.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SandboxDirectory {
    /// Absolute path of the directory listed.
    pub path: String,
    /// The sandbox work directory, where the assistant's relative paths start.
    pub work_dir: String,
    pub entries: Vec<SandboxEntry>,
    /// True when the directory has more than the entries returned.
    pub truncated: bool,
}

/// One file of the chat's sandbox, for the viewer.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SandboxFileView {
    /// Absolute path inside the sandbox.
    pub path: String,
    /// Size in bytes of the whole file.
    pub size: u64,
    /// The text, or its first [`MAX_VIEW_BYTES`] when `truncated`. `None`
    /// for a binary file.
    pub content: Option<String>,
    pub binary: bool,
    pub truncated: bool,
}

/// How a file differs from the last commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GitChangeKind {
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Untracked,
    Conflicted,
}

/// A file that differs from the repository's last commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct GitChangedFile {
    /// Path relative to the repository root.
    pub path: String,
    /// The path before a rename or copy.
    pub old_path: Option<String>,
    pub change: GitChangeKind,
    /// True when the change is staged (in the index).
    pub staged: bool,
}

/// The uncommitted changes of one repository in the sandbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct GitRepoChanges {
    /// Absolute path of the repository root.
    pub root: String,
    /// The root relative to the work directory (`.` for the work directory).
    pub name: String,
    /// The checked-out branch, or the short commit when detached.
    pub branch: Option<String>,
    /// False for a repository with no commit yet: every file is new.
    pub has_commits: bool,
    pub files: Vec<GitChangedFile>,
    /// True when more files changed than are listed.
    pub truncated: bool,
}

/// The Git changes of every repository found under the work directory.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SandboxChanges {
    pub work_dir: String,
    /// False when the sandbox has no `git`: nothing can be tracked.
    pub git_available: bool,
    /// Repositories at most three levels below the work directory, outside
    /// `node_modules`.
    pub repos: Vec<GitRepoChanges>,
}

/// Both sides of one file's diff against the last commit.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SandboxFileDiff {
    /// Absolute path of the repository root.
    pub repo: String,
    /// Path relative to the repository root.
    pub path: String,
    /// The file as last committed. `None` when it is new.
    pub original: Option<String>,
    /// The file as it is now. `None` when it was deleted.
    pub modified: Option<String>,
    /// True when either side is binary; both sides are then `None`.
    pub binary: bool,
    /// True when either side was cut at [`MAX_VIEW_BYTES`].
    pub truncated: bool,
}

/// Reads the chat's sandbox for the working tabs.
pub struct SandboxFilesService {
    link: SandboxLinkService,
}

/// The sandbox a request works on, with its id and work directory.
struct Target {
    sandbox_id: String,
    work_dir: String,
}

/// What a command printed, and how it exited.
struct Output {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

impl SandboxFilesService {
    pub fn new(link: SandboxLinkService) -> Self {
        Self { link }
    }

    /// The sandbox linked to `conversation`, started if it was asleep.
    async fn target(
        &self,
        conversation: &ai_conversations::Model,
        user_id: i32,
    ) -> Result<Target, SandboxFilesError> {
        if crate::sandbox_link::link_from_metadata(conversation.metadata.as_ref()).is_none() {
            return Err(SandboxLinkError::NotLinked {
                conversation_id: conversation.public_id.clone(),
            }
            .into());
        }
        let ready = self
            .link
            .ready_sandbox("working tabs", conversation.id, user_id)
            .await
            .map_err(|reason| SandboxFilesError::NotReady {
                conversation_id: conversation.public_id.clone(),
                reason,
            })?;
        Ok(Target {
            sandbox_id: ready.link.sandbox_id,
            work_dir: ready.work_dir,
        })
    }

    async fn run(
        &self,
        conversation: &ai_conversations::Model,
        target: &Target,
        user_id: i32,
        operation: &str,
        cmd: Vec<String>,
    ) -> Result<Output, SandboxFilesError> {
        let options = ExecOptions {
            cmd,
            env: HashMap::new(),
            cwd: None,
        };
        let result = tokio::time::timeout(
            Duration::from_secs(COMMAND_TIMEOUT_SECS),
            self.link
                .sandboxes
                .exec(&target.sandbox_id, user_id, options),
        )
        .await
        .map_err(|_| SandboxFilesError::Timeout {
            conversation_id: conversation.public_id.clone(),
            operation: operation.to_string(),
            secs: COMMAND_TIMEOUT_SECS,
        })?
        .map_err(|error| SandboxFilesError::Command {
            conversation_id: conversation.public_id.clone(),
            operation: operation.to_string(),
            reason: error.to_string(),
        })?;
        Ok(Output {
            exit_code: result.exit_code,
            stdout: result.stdout,
            stderr: result.stderr,
        })
    }

    /// One directory, `path` resolved against the work directory (the work
    /// directory itself when `None`).
    pub async fn list(
        &self,
        conversation: &ai_conversations::Model,
        user_id: i32,
        path: Option<&str>,
    ) -> Result<SandboxDirectory, SandboxFilesError> {
        let target = self.target(conversation, user_id).await?;
        let dir = sandbox_path(conversation, &target.work_dir, path)?;
        let output = self
            .run(
                conversation,
                &target,
                user_id,
                &format!("list {dir}"),
                vec![
                    "find".into(),
                    dir.clone(),
                    "-mindepth".into(),
                    "1".into(),
                    "-maxdepth".into(),
                    "1".into(),
                    "-printf".into(),
                    "%y\\t%s\\t%f\\0".into(),
                ],
            )
            .await?;
        if output.exit_code != 0 && output.stdout.is_empty() {
            return Err(missing_or_failed(
                conversation,
                &dir,
                &format!("list {dir}"),
                &output,
            ));
        }
        let (entries, truncated) = parse_listing(&dir, &output.stdout, MAX_LIST_ENTRIES);
        Ok(SandboxDirectory {
            path: dir,
            work_dir: target.work_dir,
            entries,
            truncated,
        })
    }

    /// One file for the viewer, `path` resolved against the work directory.
    pub async fn read(
        &self,
        conversation: &ai_conversations::Model,
        user_id: i32,
        path: &str,
    ) -> Result<SandboxFileView, SandboxFilesError> {
        let target = self.target(conversation, user_id).await?;
        let file = sandbox_path(conversation, &target.work_dir, Some(path))?;
        self.read_file(conversation, &target, user_id, &file).await
    }

    async fn read_file(
        &self,
        conversation: &ai_conversations::Model,
        target: &Target,
        user_id: i32,
        file: &str,
    ) -> Result<SandboxFileView, SandboxFilesError> {
        // `stat` answers whether it is a regular file and how big it is in one
        // go; `head` then reads no more than the viewer will show.
        let script = "test -f \"$1\" || { echo 'not a regular file' >&2; exit 3; }; \
                      stat -c %s \"$1\"; head -c \"$2\" \"$1\"";
        let output = self
            .run(
                conversation,
                target,
                user_id,
                &format!("read {file}"),
                vec![
                    "sh".into(),
                    "-c".into(),
                    script.into(),
                    "sh".into(),
                    file.to_string(),
                    (MAX_VIEW_BYTES + 1).to_string(),
                ],
            )
            .await?;
        if output.exit_code != 0 {
            return Err(missing_or_failed(
                conversation,
                file,
                &format!("read {file}"),
                &output,
            ));
        }
        let (size_line, body) = output
            .stdout
            .split_once('\n')
            .unwrap_or((&output.stdout, ""));
        let size = size_line.trim().parse::<u64>().unwrap_or(body.len() as u64);
        let (content, binary, truncated) = view_text(body, MAX_VIEW_BYTES);
        Ok(SandboxFileView {
            path: file.to_string(),
            size,
            content,
            binary,
            truncated,
        })
    }

    /// The uncommitted changes of the repositories under the work directory.
    pub async fn changes(
        &self,
        conversation: &ai_conversations::Model,
        user_id: i32,
    ) -> Result<SandboxChanges, SandboxFilesError> {
        let target = self.target(conversation, user_id).await?;
        self.changes_of(conversation, &target, user_id).await
    }

    async fn changes_of(
        &self,
        conversation: &ai_conversations::Model,
        target: &Target,
        user_id: i32,
    ) -> Result<SandboxChanges, SandboxFilesError> {
        let output = self
            .run(
                conversation,
                target,
                user_id,
                "read the Git changes",
                vec![
                    "sh".into(),
                    "-c".into(),
                    CHANGES_SCRIPT.into(),
                    "sh".into(),
                    target.work_dir.clone(),
                    REPO_SEARCH_DEPTH.to_string(),
                    MAX_REPOS.to_string(),
                ],
            )
            .await?;
        if output.exit_code != 0 {
            return Err(SandboxFilesError::Command {
                conversation_id: conversation.public_id.clone(),
                operation: "read the Git changes".into(),
                reason: command_reason(&output),
            });
        }
        Ok(parse_changes(
            &target.work_dir,
            &output.stdout,
            MAX_CHANGED_FILES,
        ))
    }

    /// Both sides of one file's diff. `repo` is a repository root (absolute,
    /// or relative to the work directory); `path` and `old_path` are relative
    /// to it.
    pub async fn diff(
        &self,
        conversation: &ai_conversations::Model,
        user_id: i32,
        repo: &str,
        path: &str,
        old_path: Option<&str>,
    ) -> Result<SandboxFileDiff, SandboxFilesError> {
        let target = self.target(conversation, user_id).await?;
        let root = sandbox_path(conversation, &target.work_dir, Some(repo))?;
        let path = repo_relative_path(conversation, path)?;
        let old_path = old_path
            .map(|old| repo_relative_path(conversation, old))
            .transpose()?;
        let committed = old_path.as_deref().unwrap_or(&path);
        let original = self
            .run(
                conversation,
                &target,
                user_id,
                &format!("read the committed {committed}"),
                vec![
                    "sh".into(),
                    "-c".into(),
                    "git -c safe.directory='*' -C \"$1\" cat-file -e \"HEAD:$2\" 2>/dev/null || exit 4; \
                     git -c safe.directory='*' -C \"$1\" show \"HEAD:$2\" | head -c \"$3\""
                        .into(),
                    "sh".into(),
                    root.clone(),
                    committed.to_string(),
                    (MAX_VIEW_BYTES + 1).to_string(),
                ],
            )
            .await?;
        let original = match original.exit_code {
            0 => Some(original.stdout),
            // Not in the last commit (a new file), or no commit at all.
            4 => None,
            _ => {
                return Err(SandboxFilesError::Command {
                    conversation_id: conversation.public_id.clone(),
                    operation: format!("read the committed {committed}"),
                    reason: command_reason(&original),
                })
            }
        };
        let current = format!("{}/{}", root.trim_end_matches('/'), path);
        let modified = match self
            .read_file(conversation, &target, user_id, &current)
            .await
        {
            Ok(view) if view.binary => return Ok(binary_diff(root, path)),
            Ok(view) => Some((view.content.unwrap_or_default(), view.truncated)),
            Err(SandboxFilesError::NotFound { .. }) => None,
            Err(error) => return Err(error),
        };
        let (original_text, original_binary, original_truncated) = match original {
            Some(text) => {
                let (content, binary, truncated) = view_text(&text, MAX_VIEW_BYTES);
                (content, binary, truncated)
            }
            None => (None, false, false),
        };
        if original_binary {
            return Ok(binary_diff(root, path));
        }
        let modified_truncated = modified.as_ref().is_some_and(|(_, cut)| *cut);
        Ok(SandboxFileDiff {
            repo: root,
            path,
            original: original_text,
            modified: modified.map(|(text, _)| text),
            binary: false,
            truncated: original_truncated || modified_truncated,
        })
    }

    /// Put `path` (the work directory when `None`) under Git so its changes
    /// show from now on: a `.gitignore` for dependencies, builds and `.env`
    /// files when it has none, then a first commit of what is there. Refuses a
    /// folder already inside a repository, whose history is the user's.
    /// Returns the sandbox id and the folder, for the audit log, with the
    /// changes as they are now.
    pub async fn start_tracking(
        &self,
        conversation: &ai_conversations::Model,
        user_id: i32,
        path: Option<&str>,
    ) -> Result<(String, String, SandboxChanges), SandboxFilesError> {
        let target = self.target(conversation, user_id).await?;
        let dir = sandbox_path(conversation, &target.work_dir, path)?;
        let output = self
            .run(
                conversation,
                &target,
                user_id,
                &format!("start tracking changes in {dir}"),
                vec![
                    "sh".into(),
                    "-c".into(),
                    TRACK_SCRIPT.into(),
                    "sh".into(),
                    dir.clone(),
                ],
            )
            .await?;
        match output.exit_code {
            0 => {}
            3 => {
                return Err(SandboxFilesError::NotFound {
                    conversation_id: conversation.public_id.clone(),
                    path: dir,
                })
            }
            4 => {
                return Err(SandboxFilesError::AlreadyTracked {
                    conversation_id: conversation.public_id.clone(),
                    path: dir,
                })
            }
            _ => {
                return Err(SandboxFilesError::Command {
                    conversation_id: conversation.public_id.clone(),
                    operation: format!("start tracking changes in {dir}"),
                    reason: command_reason(&output),
                })
            }
        }
        let changes = self.changes_of(conversation, &target, user_id).await?;
        Ok((target.sandbox_id, dir, changes))
    }
}

/// Finds the repositories under `$1` (at most `$2` levels down, outside
/// `node_modules`, at most `$3` of them) and prints, for each, a header line
/// `<RS>root<TAB>branch<TAB>has_commits` followed by `git status -z`.
const CHANGES_SCRIPT: &str = r#"
command -v git >/dev/null 2>&1 || { printf 'NOGIT\n'; exit 0; }
find "$1" -maxdepth "$2" \( -name node_modules -o -name .cache -o -name .venv \) -prune -o -name .git -print 2>/dev/null \
  | head -n "$3" | while IFS= read -r dotgit; do
  root=$(dirname "$dotgit")
  branch=$(git -c safe.directory='*' -C "$root" symbolic-ref --short -q HEAD 2>/dev/null \
    || git -c safe.directory='*' -C "$root" rev-parse --short HEAD 2>/dev/null)
  if git -c safe.directory='*' -C "$root" rev-parse -q --verify HEAD >/dev/null 2>&1; then commits=1; else commits=0; fi
  printf '\036%s\t%s\t%s\n' "$root" "$branch" "$commits"
  git -c safe.directory='*' -C "$root" status --porcelain=v1 -z --untracked-files=all 2>/dev/null | head -c 2000000
done
"#;

/// `git init` in `$1` with a first commit of what it holds. Exit 3: no such
/// folder; exit 4: already inside a repository.
const TRACK_SCRIPT: &str = r#"
cd "$1" 2>/dev/null || exit 3
if git -c safe.directory='*' rev-parse --is-inside-work-tree >/dev/null 2>&1; then exit 4; fi
set -e
if [ ! -e .gitignore ]; then
  printf '%s\n' 'node_modules/' '.next/' 'dist/' 'build/' '.cache/' '.venv/' '.env' '.env.*' '!.env.example' > .gitignore
fi
git init -q
git add -A
git -c user.name='Temps' -c user.email='assistant@temps.invalid' commit -q --allow-empty -m 'Start tracking changes'
"#;

/// `path` as an absolute sandbox path: relative paths start at the work
/// directory, `None` is the work directory.
fn sandbox_path(
    conversation: &ai_conversations::Model,
    work_dir: &str,
    path: Option<&str>,
) -> Result<String, SandboxFilesError> {
    let raw = path.map(str::trim).filter(|p| !p.is_empty());
    let Some(raw) = raw.filter(|p| *p != ".") else {
        return Ok(work_dir.to_string());
    };
    if raw.contains(['\0', '\n', '\r']) {
        return Err(SandboxFilesError::InvalidPath {
            conversation_id: conversation.public_id.clone(),
            path: raw.escape_debug().to_string(),
            reason: "paths cannot contain NUL or line breaks".into(),
        });
    }
    let resolved = resolve_path(work_dir, raw);
    if resolved.len() > 1 {
        Ok(resolved.trim_end_matches('/').to_string())
    } else {
        Ok(resolved)
    }
}

/// A path inside a repository, as `git show HEAD:<path>` takes it.
fn repo_relative_path(
    conversation: &ai_conversations::Model,
    path: &str,
) -> Result<String, SandboxFilesError> {
    let invalid = |reason: &str| SandboxFilesError::InvalidPath {
        conversation_id: conversation.public_id.clone(),
        path: path.escape_debug().to_string(),
        reason: reason.to_string(),
    };
    if path.is_empty() {
        return Err(invalid("the path is empty"));
    }
    if path.contains(['\0', '\n', '\r']) {
        return Err(invalid("paths cannot contain NUL or line breaks"));
    }
    if path.starts_with('/') {
        return Err(invalid("give the path relative to the repository root"));
    }
    if path.split('/').any(|part| part == "..") {
        return Err(invalid("the path cannot leave the repository"));
    }
    Ok(path.to_string())
}

/// Why a command failed, from what it printed.
fn command_reason(output: &Output) -> String {
    let stderr = output.stderr.trim();
    if stderr.is_empty() {
        format!("the command exited with code {}", output.exit_code)
    } else {
        format!(
            "{} (exit code {})",
            stderr.chars().take(500).collect::<String>(),
            output.exit_code
        )
    }
}

/// `NotFound` when the command says the path is missing, `Command` otherwise.
fn missing_or_failed(
    conversation: &ai_conversations::Model,
    path: &str,
    operation: &str,
    output: &Output,
) -> SandboxFilesError {
    let stderr = output.stderr.to_ascii_lowercase();
    if stderr.contains("no such file")
        || stderr.contains("not a regular file")
        || stderr.contains("not a directory")
    {
        SandboxFilesError::NotFound {
            conversation_id: conversation.public_id.clone(),
            path: path.to_string(),
        }
    } else {
        SandboxFilesError::Command {
            conversation_id: conversation.public_id.clone(),
            operation: operation.to_string(),
            reason: command_reason(output),
        }
    }
}

/// The entries `find -printf '%y\t%s\t%f\0'` printed for `dir`, folders
/// first then files, each by name; at most `max` of them.
fn parse_listing(dir: &str, stdout: &str, max: usize) -> (Vec<SandboxEntry>, bool) {
    let base = dir.trim_end_matches('/');
    let mut entries: Vec<SandboxEntry> = stdout
        .split('\0')
        .filter_map(|record| {
            let mut fields = record.splitn(3, '\t');
            let kind = fields.next()?;
            let size = fields.next()?.parse::<u64>().unwrap_or(0);
            let name = fields.next()?;
            if name.is_empty() {
                return None;
            }
            let kind = match kind {
                "f" => SandboxEntryKind::File,
                "d" => SandboxEntryKind::Directory,
                "l" => SandboxEntryKind::Symlink,
                _ => SandboxEntryKind::Other,
            };
            Some(SandboxEntry {
                name: name.to_string(),
                path: format!("{base}/{name}"),
                kind,
                size,
            })
        })
        .collect();
    entries.sort_by(|a, b| {
        let rank = |entry: &SandboxEntry| (entry.kind != SandboxEntryKind::Directory) as u8;
        rank(a)
            .cmp(&rank(b))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    let truncated = entries.len() > max;
    entries.truncate(max);
    (entries, truncated)
}

/// The text the viewer shows of `body`: `(content, binary, truncated)`.
/// Binary when the first bytes hold a NUL or the text did not decode.
fn view_text(body: &str, max: usize) -> (Option<String>, bool, bool) {
    let sniff_end = floor_char_boundary(body, BINARY_SNIFF_BYTES.min(body.len()));
    let sniff = &body[..sniff_end];
    if sniff.contains('\0') || sniff.contains('\u{FFFD}') {
        return (None, true, body.len() > max);
    }
    if body.len() <= max {
        return (Some(body.to_string()), false, false);
    }
    let end = floor_char_boundary(body, max);
    (Some(body[..end].to_string()), false, true)
}

fn floor_char_boundary(text: &str, index: usize) -> usize {
    let mut end = index.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

fn binary_diff(repo: String, path: String) -> SandboxFileDiff {
    SandboxFileDiff {
        repo,
        path,
        original: None,
        modified: None,
        binary: true,
        truncated: false,
    }
}

/// What [`CHANGES_SCRIPT`] printed, as one entry per repository.
fn parse_changes(work_dir: &str, stdout: &str, max_files: usize) -> SandboxChanges {
    if stdout.starts_with("NOGIT") {
        return SandboxChanges {
            work_dir: work_dir.to_string(),
            git_available: false,
            repos: Vec::new(),
        };
    }
    let mut repos: Vec<GitRepoChanges> = stdout
        .split(REPO_MARKER)
        .skip(1)
        .filter_map(|section| {
            let (header, status) = section.split_once('\n').unwrap_or((section, ""));
            let mut fields = header.split('\t');
            let root = fields.next()?.to_string();
            if root.is_empty() {
                return None;
            }
            let branch = fields
                .next()
                .map(str::trim)
                .filter(|b| !b.is_empty())
                .map(str::to_string);
            let has_commits = fields.next() == Some("1");
            let (files, truncated) = parse_status(status, max_files);
            Some(GitRepoChanges {
                name: relative_name(work_dir, &root),
                root,
                branch,
                has_commits,
                files,
                truncated,
            })
        })
        .collect();
    repos.sort_by(|a, b| a.name.cmp(&b.name));
    SandboxChanges {
        work_dir: work_dir.to_string(),
        git_available: true,
        repos,
    }
}

/// `root` relative to `work_dir`, `.` for the work directory itself.
fn relative_name(work_dir: &str, root: &str) -> String {
    let base = work_dir.trim_end_matches('/');
    if root == base {
        return ".".to_string();
    }
    root.strip_prefix(&format!("{base}/"))
        .unwrap_or(root)
        .to_string()
}

/// The files of `git status --porcelain=v1 -z`, at most `max` of them.
fn parse_status(status: &str, max: usize) -> (Vec<GitChangedFile>, bool) {
    let mut files = Vec::new();
    let mut truncated = false;
    let mut records = status.split('\0');
    while let Some(record) = records.next() {
        if record.len() < 4 || !record.is_char_boundary(2) || !record.is_char_boundary(3) {
            continue;
        }
        let (index, worktree) = (record.as_bytes()[0], record.as_bytes()[1]);
        let path = record[3..].to_string();
        // A rename or copy is followed by the path it came from.
        let old_path = if matches!(index, b'R' | b'C') || matches!(worktree, b'R' | b'C') {
            records.next().map(str::to_string)
        } else {
            None
        };
        if files.len() == max {
            truncated = true;
            break;
        }
        files.push(GitChangedFile {
            path,
            old_path,
            change: change_kind(index, worktree),
            staged: !matches!(index, b' ' | b'?' | b'!'),
        });
    }
    (files, truncated)
}

fn change_kind(index: u8, worktree: u8) -> GitChangeKind {
    match (index, worktree) {
        (b'?', b'?') => GitChangeKind::Untracked,
        (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D') => GitChangeKind::Conflicted,
        _ => {
            let letters = [worktree, index];
            if letters.contains(&b'D') {
                GitChangeKind::Deleted
            } else if letters.contains(&b'R') {
                GitChangeKind::Renamed
            } else if letters.contains(&b'C') {
                GitChangeKind::Copied
            } else if letters.contains(&b'A') {
                GitChangeKind::Added
            } else if letters.contains(&b'T') {
                GitChangeKind::TypeChanged
            } else {
                GitChangeKind::Modified
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conversation() -> ai_conversations::Model {
        let now = chrono::Utc::now();
        ai_conversations::Model {
            id: 7,
            public_id: "conv_files".to_string(),
            project_id: None,
            application_id: None,
            context_type: "assistant".to_string(),
            context_id: "assistant_1".to_string(),
            title: None,
            status: "active".to_string(),
            kind: "ai".to_string(),
            created_by: 1,
            metadata: None,
            created_at: now,
            last_activity_at: now,
            ai_provider: "gateway_key:1".to_string(),
            ai_model: "model".to_string(),
            ai_thinking_level: None,
            ai_permission_mode: "auto".to_string(),
            cli_session_id: None,
            cli_session_fingerprint: None,
            turn_status: "idle".to_string(),
            active_turn_id: None,
            last_turn_id: None,
            turn_started_at: None,
            next_step_suggestions: None,
        }
    }

    #[test]
    fn a_listing_puts_folders_first_and_sorts_by_name() {
        let stdout = "f\t12\tb.ts\0d\t4096\tsrc\0f\t3\tA.md\0l\t9\tlink\0d\t4096\t.git\0";
        let (entries, truncated) = parse_listing("/w", stdout, 100);
        assert!(!truncated);
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, [".git", "src", "A.md", "b.ts", "link"]);
        assert_eq!(entries[1].path, "/w/src");
        assert_eq!(entries[1].kind, SandboxEntryKind::Directory);
        assert_eq!(entries[4].kind, SandboxEntryKind::Symlink);
        assert_eq!(entries[3].size, 12);
    }

    #[test]
    fn a_listing_keeps_names_with_spaces_and_tabs() {
        let (entries, _) = parse_listing("/w/", "f\t1\tmy file\twith tab\0", 10);
        assert_eq!(entries[0].name, "my file\twith tab");
        assert_eq!(entries[0].path, "/w/my file\twith tab");
    }

    #[test]
    fn a_long_listing_is_cut_and_says_so() {
        let stdout: String = (0..5).map(|i| format!("f\t1\tf{i}\0")).collect();
        let (entries, truncated) = parse_listing("/w", &stdout, 3);
        assert_eq!(entries.len(), 3);
        assert!(truncated);
    }

    #[test]
    fn text_is_shown_and_binary_is_not() {
        assert_eq!(
            view_text("hello\n", 100),
            (Some("hello\n".into()), false, false)
        );
        assert_eq!(view_text("PK\u{0}\u{3}", 100), (None, true, false));
        assert_eq!(view_text("bad \u{FFFD} bytes", 100), (None, true, false));
    }

    #[test]
    fn a_big_file_is_cut_on_a_character_boundary() {
        let (content, binary, truncated) = view_text("ééé", 3);
        assert_eq!(content.as_deref(), Some("é"));
        assert!(!binary);
        assert!(truncated);
    }

    #[test]
    fn status_reads_every_kind_of_change() {
        let status = " M src/app.ts\0A  new.ts\0 D gone.ts\0R  to.ts\0from.ts\0?? scratch.txt\0UU both.ts\0MM staged-and-not.ts\0";
        let (files, truncated) = parse_status(status, 100);
        assert!(!truncated);
        let summary: Vec<_> = files
            .iter()
            .map(|f| (f.path.as_str(), f.change, f.staged, f.old_path.as_deref()))
            .collect();
        assert_eq!(
            summary,
            [
                ("src/app.ts", GitChangeKind::Modified, false, None),
                ("new.ts", GitChangeKind::Added, true, None),
                ("gone.ts", GitChangeKind::Deleted, false, None),
                ("to.ts", GitChangeKind::Renamed, true, Some("from.ts")),
                ("scratch.txt", GitChangeKind::Untracked, false, None),
                ("both.ts", GitChangeKind::Conflicted, true, None),
                ("staged-and-not.ts", GitChangeKind::Modified, true, None),
            ]
        );
    }

    #[test]
    fn status_stops_at_the_file_limit() {
        let status: String = (0..10).map(|i| format!("?? f{i}\0")).collect();
        let (files, truncated) = parse_status(&status, 4);
        assert_eq!(files.len(), 4);
        assert!(truncated);
    }

    #[test]
    fn changes_name_each_repository_from_the_work_directory() {
        let stdout = "\u{1e}/home/temps/workspace/app\tmain\t1\n M page.tsx\0\
                      \u{1e}/home/temps/workspace\t\t0\n?? notes.md\0";
        let changes = parse_changes("/home/temps/workspace", stdout, 100);
        assert!(changes.git_available);
        let repos: Vec<_> = changes
            .repos
            .iter()
            .map(|r| {
                (
                    r.name.as_str(),
                    r.branch.as_deref(),
                    r.has_commits,
                    r.files.len(),
                )
            })
            .collect();
        assert_eq!(
            repos,
            [(".", None, false, 1), ("app", Some("main"), true, 1)]
        );
    }

    #[test]
    fn a_sandbox_without_git_says_so() {
        let changes = parse_changes("/w", "NOGIT\n", 100);
        assert!(!changes.git_available);
        assert!(changes.repos.is_empty());
    }

    #[test]
    fn a_work_directory_with_no_repository_has_no_changes() {
        let changes = parse_changes("/w", "", 100);
        assert!(changes.git_available);
        assert!(changes.repos.is_empty());
    }

    #[test]
    fn paths_resolve_against_the_work_directory() {
        let c = conversation();
        assert_eq!(sandbox_path(&c, "/w", None).unwrap(), "/w");
        assert_eq!(sandbox_path(&c, "/w", Some("  ")).unwrap(), "/w");
        assert_eq!(sandbox_path(&c, "/w", Some(".")).unwrap(), "/w");
        assert_eq!(sandbox_path(&c, "/w", Some("src/")).unwrap(), "/w/src");
        assert_eq!(sandbox_path(&c, "/w", Some("/etc")).unwrap(), "/etc");
        assert_eq!(sandbox_path(&c, "/w", Some("/")).unwrap(), "/");
        assert!(matches!(
            sandbox_path(&c, "/w", Some("a\nb")),
            Err(SandboxFilesError::InvalidPath { .. })
        ));
    }

    #[test]
    fn a_diff_path_must_stay_inside_its_repository() {
        let c = conversation();
        assert_eq!(repo_relative_path(&c, "src/a.ts").unwrap(), "src/a.ts");
        for bad in ["", "/etc/passwd", "../outside", "a/../../b", "a\0b"] {
            assert!(
                matches!(
                    repo_relative_path(&c, bad),
                    Err(SandboxFilesError::InvalidPath { .. })
                ),
                "{bad:?} should be refused"
            );
        }
    }

    #[test]
    fn a_failure_that_names_a_missing_path_is_not_found() {
        let c = conversation();
        let output = Output {
            exit_code: 1,
            stdout: String::new(),
            stderr: "find: '/w/nope': No such file or directory".into(),
        };
        assert!(matches!(
            missing_or_failed(&c, "/w/nope", "list /w/nope", &output),
            SandboxFilesError::NotFound { .. }
        ));
        let output = Output {
            exit_code: 1,
            stdout: String::new(),
            stderr: "Permission denied".into(),
        };
        let error = missing_or_failed(&c, "/root", "list /root", &output);
        assert!(error.to_string().contains("Permission denied"));
        assert!(error.to_string().contains("conv_files"));
    }
}
