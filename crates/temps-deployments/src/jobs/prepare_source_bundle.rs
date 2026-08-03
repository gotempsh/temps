//! Prepare an uploaded source archive for the normal preset build pipeline.

use async_trait::async_trait;
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use temps_core::{JobResult, WorkflowContext, WorkflowError, WorkflowTask};

#[derive(Debug)]
pub struct PrepareSourceBundleJob {
    job_id: String,
    archive_path: PathBuf,
    project_slug: String,
}

impl PrepareSourceBundleJob {
    pub fn new(job_id: String, archive_path: PathBuf, project_slug: String) -> Self {
        Self {
            job_id,
            archive_path,
            project_slug,
        }
    }

    fn validate_path(path: &Path) -> Result<(), WorkflowError> {
        if path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(WorkflowError::InvalidArchiveEntry {
                path: path.display().to_string(),
                reason: "entry path escapes the source root".to_string(),
            });
        }
        Ok(())
    }

    fn extract(&self, target: &Path) -> Result<u32, WorkflowError> {
        const MAX_FILES: u32 = 20_000;
        const MAX_ENTRY_BYTES: u64 = 500 * 1024 * 1024;
        const MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;

        let file = fs::File::open(&self.archive_path).map_err(WorkflowError::IoError)?;
        let mut archive = zip::ZipArchive::new(file).map_err(|error| {
            WorkflowError::JobExecutionFailed(format!(
                "Failed to open source archive '{}': {error}",
                self.archive_path.display()
            ))
        })?;
        let mut count = 0u32;
        let mut total = 0u64;
        for index in 0..archive.len() {
            let entry = archive.by_index(index).map_err(|error| {
                WorkflowError::JobExecutionFailed(format!(
                    "Failed to read ZIP entry {index}: {error}"
                ))
            })?;
            let enclosed =
                entry
                    .enclosed_name()
                    .ok_or_else(|| WorkflowError::InvalidArchiveEntry {
                        path: entry.name().to_string(),
                        reason: "entry path escapes the source root".to_string(),
                    })?;
            Self::validate_path(&enclosed)?;
            if entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
            {
                return Err(WorkflowError::InvalidArchiveEntry {
                    path: entry.name().to_string(),
                    reason: "symbolic links are not allowed".to_string(),
                });
            }
            if entry.is_dir() {
                fs::create_dir_all(target.join(&enclosed)).map_err(WorkflowError::IoError)?;
                continue;
            }
            count = count.saturating_add(1);
            total = total.saturating_add(entry.size());
            if count > MAX_FILES || entry.size() > MAX_ENTRY_BYTES || total > MAX_TOTAL_BYTES {
                return Err(WorkflowError::JobExecutionFailed(
                    "Source archive exceeds extraction safety limits".to_string(),
                ));
            }
            let destination = target.join(&enclosed);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(WorkflowError::IoError)?;
            }
            let mut output = fs::File::create(&destination).map_err(WorkflowError::IoError)?;
            io::copy(&mut entry.take(MAX_ENTRY_BYTES + 1), &mut output)
                .map_err(WorkflowError::IoError)?;
        }
        Ok(count)
    }
}

#[async_trait]
impl WorkflowTask for PrepareSourceBundleJob {
    fn job_id(&self) -> &str {
        &self.job_id
    }
    fn name(&self) -> &str {
        "Prepare Uploaded Source"
    }
    fn description(&self) -> &str {
        "Securely extracts uploaded source code for the preset builder"
    }

    async fn execute(&self, mut context: WorkflowContext) -> Result<JobResult, WorkflowError> {
        let work_root = context.work_dir.clone().unwrap_or_else(std::env::temp_dir);
        let target = work_root.join(format!(
            "uploaded-source-{}-{}",
            self.project_slug,
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&target).map_err(WorkflowError::IoError)?;
        let file_count = self.extract(&target)?;
        if file_count == 0 {
            return Err(WorkflowError::JobExecutionFailed(
                "Uploaded source archive is empty".to_string(),
            ));
        }

        context.set_output(
            &self.job_id,
            "repo_dir",
            target.to_string_lossy().to_string(),
        )?;
        context.set_output(&self.job_id, "checkout_ref", "uploaded-source")?;
        context.set_output(&self.job_id, "repo_owner", "drop")?;
        context.set_output(&self.job_id, "repo_name", &self.project_slug)?;
        context.set_artifact(&self.job_id, "source_code", target.clone());
        context.work_dir = target.parent().map(Path::to_path_buf);
        Ok(JobResult::success(context))
    }

    async fn validate_prerequisites(
        &self,
        _context: &WorkflowContext,
    ) -> Result<(), WorkflowError> {
        if !self.archive_path.is_file() {
            return Err(WorkflowError::JobValidationFailed(format!(
                "Uploaded source archive '{}' does not exist",
                self.archive_path.display()
            )));
        }
        Ok(())
    }

    async fn cleanup(&self, context: &WorkflowContext) -> Result<(), WorkflowError> {
        if let Some(work_dir) = &context.work_dir {
            if work_dir
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("uploaded-source-"))
            {
                fs::remove_dir_all(work_dir).map_err(WorkflowError::IoError)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_parent_directory_entries() {
        let error = PrepareSourceBundleJob::validate_path(Path::new("../secret"));
        assert!(matches!(
            error,
            Err(WorkflowError::InvalidArchiveEntry { .. })
        ));
    }
}
