//! Capture Source Files Job
//!
//! Uploads raw application source from the git checkout so native (Go, Rust,
//! Python, …) stack frames can be shown with source context — the counterpart
//! to [`super::capture_source_maps`], which extracts `.map` files from the built
//! image for JavaScript. Native source is NOT in the compiled image (the binary
//! strips it), so this reads the git checkout instead.
//!
//! By default it captures from the deployment's **Docker build context** (the
//! exact directory the image was built from) — the correct root for Dockerfile
//! deploys and monorepos, since frame paths are relative to what was copied in.
//! `.temps.yaml sourceContext.root` and the project `error_source_root` setting
//! override that default (see [`resolve_capture_root`]).
//!
//! Runs as a post-deployment job (never blocks the pipeline) and is only
//! scheduled when the project has opted into source context, so it is a no-op
//! for everyone else.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use temps_core::{JobResult, WorkflowContext, WorkflowError, WorkflowTask};
use temps_error_tracking::services::SourceMapService;
use temps_logs::{LogLevel, LogService};
use tracing::{debug, info, warn};

/// Directory names never walked for source (build output, deps, VCS metadata).
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "vendor",
    "target",
    "dist",
    "build",
    ".next",
    ".nuxt",
    ".venv",
    "venv",
    "__pycache__",
    ".temps",
];

/// Safety bounds so a huge repo can't exhaust memory/storage on a small box.
const MAX_FILES: usize = 5000;
const MAX_FILE_SIZE: u64 = 2 * 1024 * 1024; // 2 MB per source file

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureSourceFilesOutput {
    pub source_files_captured: u32,
    pub total_size_bytes: u64,
    pub release: String,
}

/// Job that uploads raw source files from the git checkout for native
/// (non-source-map) stack-trace symbolication.
pub struct CaptureSourceFilesJob {
    job_id: String,
    project_id: i32,
    release: String,
    /// Job whose `repo_dir` output holds the checkout path (the DownloadRepoJob).
    download_job_id: String,
    /// Job whose `build_context` output holds the Docker build-context dir (the
    /// BuildImageJob). This is the default capture root.
    build_job_id: String,
    /// Project-level override for the capture root, relative to the checkout.
    /// `None` = default to the build context. `.temps.yaml sourceContext.root`
    /// takes precedence over this when present.
    error_source_root: Option<String>,
    /// Default file extensions (without a dot) to upload. `.temps.yaml`
    /// `sourceContext.include` overrides this when present.
    extensions: Vec<String>,
    source_map_service: Arc<SourceMapService>,
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
}

impl std::fmt::Debug for CaptureSourceFilesJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureSourceFilesJob")
            .field("job_id", &self.job_id)
            .field("project_id", &self.project_id)
            .field("release", &self.release)
            .field("download_job_id", &self.download_job_id)
            .field("build_job_id", &self.build_job_id)
            .field("error_source_root", &self.error_source_root)
            .field("extensions", &self.extensions)
            .finish()
    }
}

impl CaptureSourceFilesJob {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        job_id: String,
        project_id: i32,
        release: String,
        download_job_id: String,
        build_job_id: String,
        error_source_root: Option<String>,
        extensions: Vec<String>,
        source_map_service: Arc<SourceMapService>,
    ) -> Self {
        Self {
            job_id,
            project_id,
            release,
            download_job_id,
            build_job_id,
            error_source_root,
            extensions,
            source_map_service,
            log_id: None,
            log_service: None,
        }
    }

    pub fn with_log_id(mut self, log_id: String) -> Self {
        self.log_id = Some(log_id);
        self
    }

    pub fn with_log_service(mut self, log_service: Arc<LogService>) -> Self {
        self.log_service = Some(log_service);
        self
    }

    async fn log(&self, message: String) -> Result<(), WorkflowError> {
        let level = Self::detect_log_level(&message);
        if let (Some(log_id), Some(log_service)) = (&self.log_id, &self.log_service) {
            log_service
                .append_structured_log(log_id, level, message)
                .await
                .map_err(|e| {
                    WorkflowError::JobExecutionFailed(format!("Failed to write log: {}", e))
                })?;
        }
        Ok(())
    }

    fn detect_log_level(message: &str) -> LogLevel {
        if message.contains("✅") || message.contains("success") {
            LogLevel::Success
        } else if message.contains("❌") || message.contains("Failed") {
            LogLevel::Error
        } else if message.contains("⚠️") {
            LogLevel::Warning
        } else {
            LogLevel::Info
        }
    }
}

/// Read `.temps.yaml sourceContext` from the checkout root. Returns
/// `(root, include)`; both `None` when the file is absent, unparsable, or has
/// no `sourceContext` block.
fn read_temps_source_context(repo_dir: &Path) -> (Option<String>, Option<Vec<String>>) {
    let contents = match std::fs::read_to_string(repo_dir.join(".temps.yaml")) {
        Ok(c) => c,
        Err(_) => return (None, None),
    };
    match temps_core::TempsConfig::from_yaml(&contents) {
        Ok(cfg) => cfg
            .source_context
            .map(|sc| (sc.root, sc.include))
            .unwrap_or((None, None)),
        Err(_) => (None, None),
    }
}

/// Resolve the directory to capture source from, in precedence order:
///   1. `.temps.yaml sourceContext.root` (repo-relative)
///   2. project `error_source_root` (repo-relative)
///   3. the Docker build context (absolute, from BuildImageJob)
///   4. the checkout root
///
/// Repo-relative overrides are confined to the checkout (reject `..`/symlink
/// escapes). The build context is confined defensively; on any mismatch it
/// falls back to the checkout root rather than capturing outside it.
fn resolve_capture_root(
    repo_dir: &Path,
    build_context: Option<&Path>,
    yaml_root: Option<&str>,
    project_root: Option<&str>,
) -> Result<PathBuf, WorkflowError> {
    use crate::jobs::deploy_compose::canonicalize_confined_repo_path;

    for (candidate, field) in [
        (yaml_root, "sourceContext.root"),
        (project_root, "error_source_root"),
    ] {
        if let Some(rel) = candidate.map(str::trim).filter(|s| !s.is_empty()) {
            return canonicalize_confined_repo_path(repo_dir, Path::new(rel), field);
        }
    }

    if let Some(bc) = build_context {
        if let (Ok(canon_repo), Ok(canon_bc)) =
            (std::fs::canonicalize(repo_dir), std::fs::canonicalize(bc))
        {
            if canon_bc.starts_with(&canon_repo) {
                return Ok(canon_bc);
            }
        }
    }

    canonicalize_confined_repo_path(repo_dir, Path::new("."), "checkout")
}

/// Recursively collect files under `root` whose extension is in `exts`,
/// skipping [`SKIP_DIRS`]. Returns `(relative_path, absolute_path)` pairs, with
/// the relative path using forward slashes (to match stack-frame filenames).
/// Stops at [`MAX_FILES`].
fn collect_source_files(root: &Path, exts: &[String]) -> Vec<(String, PathBuf)> {
    fn walk(dir: &Path, root: &Path, exts: &[String], out: &mut Vec<(String, PathBuf)>) {
        if out.len() >= MAX_FILES {
            return;
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            if out.len() >= MAX_FILES {
                return;
            }
            let path = entry.path();
            // Never follow symlinks — a repo could symlink to a host path
            // (/etc, /proc, secrets) and exfiltrate it into stored source.
            // `is_symlink` uses lstat, so it does not traverse the link.
            if path.is_symlink() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if SKIP_DIRS.contains(&name.as_ref()) {
                    continue;
                }
                walk(&path, root, exts, out);
            } else {
                let matches = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| exts.iter().any(|want| want.eq_ignore_ascii_case(e)))
                    .unwrap_or(false);
                if matches {
                    if let Ok(rel) = path.strip_prefix(root) {
                        let rel = rel.to_string_lossy().replace('\\', "/");
                        out.push((rel, path));
                    }
                }
            }
        }
    }

    let mut out = Vec::new();
    walk(root, root, exts, &mut out);
    out
}

#[async_trait]
impl WorkflowTask for CaptureSourceFilesJob {
    fn job_id(&self) -> &str {
        &self.job_id
    }

    fn name(&self) -> &str {
        "Capture Source Files"
    }

    fn description(&self) -> &str {
        "Upload application source from the checkout for native error symbolication"
    }

    async fn execute(&self, mut context: WorkflowContext) -> Result<JobResult, WorkflowError> {
        info!(
            "Starting source file capture for project {} (release: {})",
            self.project_id, self.release
        );
        self.log("📄 Capturing source files for native symbolication...".to_string())
            .await?;

        // The checkout dir is the DownloadRepoJob's `repo_dir` output. If it is
        // gone (e.g. an image deploy with no checkout), there is nothing to do.
        let repo_dir = match context.get_output::<String>(&self.download_job_id, "repo_dir")? {
            Some(dir) => PathBuf::from(dir),
            None => {
                self.log("⚠️ No checkout available; skipping source file capture".to_string())
                    .await?;
                return Ok(JobResult::success(context));
            }
        };

        // Default capture root: the Docker build context — the exact directory
        // the image was built from (BuildImageJob's `build_context` output).
        // This is the correct root for Dockerfile deploys and monorepos, since
        // frame paths are reported relative to what was copied into the build.
        let build_context = context
            .get_output::<String>(&self.build_job_id, "build_context")?
            .map(PathBuf::from);

        // `.temps.yaml sourceContext` (read from the checkout root) can override
        // the root and the extension set per-repo.
        let (yaml_root, yaml_include) = read_temps_source_context(&repo_dir);

        // Precedence: .temps.yaml root > project error_source_root > build
        // context > checkout root. Repo-relative overrides are confined to the
        // checkout (reject `..` and symlink escapes).
        let source_root = match resolve_capture_root(
            &repo_dir,
            build_context.as_deref(),
            yaml_root.as_deref(),
            self.error_source_root.as_deref(),
        ) {
            Ok(p) => p,
            Err(e) => {
                self.log(format!(
                    "⚠️ Source root not usable ({}); skipping source capture",
                    e
                ))
                .await?;
                return Ok(JobResult::success(context));
            }
        };
        self.log(format!(
            "📂 Capturing source from {}",
            source_root.display()
        ))
        .await?;

        let extensions = yaml_include.unwrap_or_else(|| self.extensions.clone());
        let files = collect_source_files(&source_root, &extensions);
        if files.len() >= MAX_FILES {
            self.log(format!(
                "⚠️ Source file cap reached ({} files); uploading the first {}",
                MAX_FILES, MAX_FILES
            ))
            .await?;
        }

        let mut captured: u32 = 0;
        let mut total_size: u64 = 0;
        let mut skipped_large: u32 = 0;

        for (rel_path, abs_path) in files {
            let content = match tokio::fs::read(&abs_path).await {
                Ok(c) => c,
                Err(e) => {
                    warn!("Failed to read {}: {}", abs_path.display(), e);
                    continue;
                }
            };
            if content.len() as u64 > MAX_FILE_SIZE {
                skipped_large += 1;
                continue;
            }
            total_size += content.len() as u64;

            match self
                .source_map_service
                .upload_source_file(self.project_id, &self.release, &rel_path, content)
                .await
            {
                Ok(_) => captured += 1,
                Err(e) => {
                    warn!("Failed to upload source file '{}': {}", rel_path, e);
                }
            }
        }

        if skipped_large > 0 {
            self.log(format!(
                "⚠️ Skipped {} file(s) larger than {} bytes",
                skipped_large, MAX_FILE_SIZE
            ))
            .await?;
        }

        self.log(format!(
            "✅ Captured {} source file(s) for release '{}' ({} bytes)",
            captured, self.release, total_size
        ))
        .await?;

        // Drop source for releases no longer tied to an active deployment.
        match self
            .source_map_service
            .delete_stale_source_files(self.project_id)
            .await
        {
            Ok(n) if n > 0 => {
                debug!(
                    "Removed {} stale source file(s) for project {}",
                    n, self.project_id
                );
            }
            Ok(_) => {}
            Err(e) => warn!("Stale source file cleanup failed: {}", e),
        }

        context.set_output(&self.job_id, "source_files_captured", captured)?;
        context.set_output(&self.job_id, "total_size_bytes", total_size)?;
        Ok(JobResult::success(context))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_skips_build_and_vcs_dirs_and_filters_extensions() {
        let root = std::env::temp_dir().join(format!("tsf-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("internal/gateway")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/foo")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("main.go"), "package main").unwrap();
        std::fs::write(root.join("internal/gateway/mw.go"), "package gateway").unwrap();
        std::fs::write(root.join("README.md"), "# docs").unwrap();
        std::fs::write(root.join("node_modules/foo/index.go"), "skip me").unwrap();
        std::fs::write(root.join(".git/config"), "skip").unwrap();

        let exts = vec!["go".to_string()];
        let mut found: Vec<String> = collect_source_files(&root, &exts)
            .into_iter()
            .map(|(rel, _)| rel)
            .collect();
        found.sort();

        assert_eq!(found, vec!["internal/gateway/mw.go", "main.go"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn collect_does_not_follow_symlinks_out_of_the_tree() {
        let root = std::env::temp_dir().join(format!("tsf-sym-{}", std::process::id()));
        let outside = std::env::temp_dir().join(format!("tsf-out-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(root.join("real.go"), "package x").unwrap();
        std::fs::write(outside.join("secret.go"), "SECRET").unwrap();
        // A symlink inside the repo pointing at an outside directory must not be
        // followed (host-file exfiltration guard).
        std::os::unix::fs::symlink(&outside, root.join("evil")).unwrap();

        let found: Vec<String> = collect_source_files(&root, &["go".to_string()])
            .into_iter()
            .map(|(rel, _)| rel)
            .collect();

        assert!(found.contains(&"real.go".to_string()));
        assert!(
            !found.iter().any(|f| f.contains("secret")),
            "symlinked out-of-tree file must not be collected: {found:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    // --- capture-root resolution (the error_source_root / build-context logic) ---

    fn tmp_repo(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("tsf-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn canon(p: &Path) -> PathBuf {
        std::fs::canonicalize(p).unwrap()
    }

    #[test]
    fn resolve_root_precedence_yaml_then_project_then_context_then_root() {
        let repo = tmp_repo("prec");
        for d in ["services/api", "services/web", "ctx"] {
            std::fs::create_dir_all(repo.join(d)).unwrap();
        }
        let ctx = repo.join("ctx");

        // 1. .temps.yaml root wins over everything.
        let r = resolve_capture_root(
            &repo,
            Some(&ctx),
            Some("services/api"),
            Some("services/web"),
        )
        .unwrap();
        assert_eq!(r, canon(&repo.join("services/api")));

        // 2. project error_source_root wins when there is no yaml root.
        let r = resolve_capture_root(&repo, Some(&ctx), None, Some("services/web")).unwrap();
        assert_eq!(r, canon(&repo.join("services/web")));

        // 3. Docker build context is the default.
        let r = resolve_capture_root(&repo, Some(&ctx), None, None).unwrap();
        assert_eq!(r, canon(&ctx));

        // 4. Checkout root when there is no build context either.
        let r = resolve_capture_root(&repo, None, None, None).unwrap();
        assert_eq!(r, canon(&repo));

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn resolve_root_rejects_traversal_overrides() {
        let repo = tmp_repo("trav");
        assert!(resolve_capture_root(&repo, None, None, Some("../../etc")).is_err());
        assert!(resolve_capture_root(&repo, None, Some(".."), None).is_err());
        assert!(resolve_capture_root(&repo, None, Some("a/../../b"), None).is_err());
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn resolve_root_ignores_empty_overrides() {
        let repo = tmp_repo("empty");
        std::fs::create_dir_all(repo.join("ctx")).unwrap();
        let ctx = repo.join("ctx");
        // whitespace/empty overrides are treated as unset → fall through to context.
        let r = resolve_capture_root(&repo, Some(&ctx), Some("   "), Some("")).unwrap();
        assert_eq!(r, canon(&ctx));
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn resolve_root_build_context_outside_repo_falls_back_to_root() {
        let repo = tmp_repo("bc-in");
        let outside = tmp_repo("bc-out");
        // A build context that resolves outside the checkout is rejected; capture
        // falls back to the checkout root rather than reading outside it.
        let r = resolve_capture_root(&repo, Some(&outside), None, None).unwrap();
        assert_eq!(r, canon(&repo));
        let _ = std::fs::remove_dir_all(&repo);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn read_temps_source_context_parses_root_and_include() {
        let repo = tmp_repo("yaml-ok");
        std::fs::write(
            repo.join(".temps.yaml"),
            "sourceContext:\n  root: services/api\n  include: [go, rs]\n",
        )
        .unwrap();
        let (root, include) = read_temps_source_context(&repo);
        assert_eq!(root.as_deref(), Some("services/api"));
        assert_eq!(include, Some(vec!["go".to_string(), "rs".to_string()]));
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn read_temps_source_context_absent_or_no_block() {
        let repo = tmp_repo("yaml-none");
        // No .temps.yaml at all.
        assert_eq!(read_temps_source_context(&repo), (None, None));
        // Present, but no sourceContext block.
        std::fs::write(
            repo.join(".temps.yaml"),
            "health:\n  path: /health\n  status: 200\n",
        )
        .unwrap();
        assert_eq!(read_temps_source_context(&repo), (None, None));
        // Malformed YAML.
        std::fs::write(repo.join(".temps.yaml"), "sourceContext: [not, a, map").unwrap();
        assert_eq!(read_temps_source_context(&repo), (None, None));
        let _ = std::fs::remove_dir_all(&repo);
    }
}
