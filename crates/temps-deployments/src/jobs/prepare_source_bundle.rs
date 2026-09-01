// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
    extraction_id: uuid::Uuid,
}

impl PrepareSourceBundleJob {
    pub fn new(job_id: String, archive_path: PathBuf, project_slug: String) -> Self {
        Self {
            job_id,
            archive_path,
            project_slug,
            extraction_id: uuid::Uuid::new_v4(),
        }
    }

    fn target_path(&self, context: &WorkflowContext) -> PathBuf {
        let work_root = context.work_dir.clone().unwrap_or_else(std::env::temp_dir);
        work_root.join(format!(
            "uploaded-source-{}-{}",
            self.project_slug, self.extraction_id
        ))
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
        if path.components().any(|component| {
            let Component::Normal(value) = component else {
                return false;
            };
            let name = value.to_string_lossy();
            name == ".git"
                || name == "node_modules"
                || name == ".env"
                || name.starts_with(".env.")
                || name.ends_with(".pem")
                || name.ends_with(".key")
                || name == "credentials.json"
        }) {
            return Err(WorkflowError::InvalidArchiveEntry {
                path: path.display().to_string(),
                reason: "sensitive or generated paths are not accepted in Drop uploads".to_string(),
            });
        }
        Ok(())
    }

    fn extract_archive(archive_path: &Path, target: &Path) -> Result<u32, WorkflowError> {
        const MAX_ENTRIES: u32 = 20_000;
        const MAX_ENTRY_BYTES: u64 = 500 * 1024 * 1024;
        const MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;

        let mut file = fs::File::open(archive_path).map_err(WorkflowError::IoError)?;
        temps_core::archive_security::validate_zip_metadata(&mut file).map_err(|error| {
            WorkflowError::JobExecutionFailed(format!("Unsafe source ZIP metadata: {error}"))
        })?;
        let mut archive = zip::ZipArchive::new(file).map_err(|error| {
            WorkflowError::JobExecutionFailed(format!(
                "Failed to open source archive '{}': {error}",
                archive_path.display()
            ))
        })?;
        let mut entry_count = 0u32;
        let mut file_count = 0u32;
        let mut total = 0u64;
        let mut written_total = 0u64;
        for index in 0..archive.len() {
            entry_count = entry_count.saturating_add(1);
            if entry_count > MAX_ENTRIES {
                return Err(WorkflowError::JobExecutionFailed(
                    "Source archive exceeds the 20,000 entry extraction limit".to_string(),
                ));
            }
            let mut entry = archive.by_index(index).map_err(|error| {
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
            file_count = file_count.saturating_add(1);
            total = total.saturating_add(entry.size());
            if entry.size() > MAX_ENTRY_BYTES || total > MAX_TOTAL_BYTES {
                return Err(WorkflowError::JobExecutionFailed(
                    "Source archive exceeds extraction safety limits".to_string(),
                ));
            }
            let destination = target.join(&enclosed);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(WorkflowError::IoError)?;
            }
            let mut output = fs::File::create(&destination).map_err(WorkflowError::IoError)?;
            let written = io::copy(&mut entry.by_ref().take(MAX_ENTRY_BYTES + 1), &mut output)
                .map_err(WorkflowError::IoError)?;
            if written > MAX_ENTRY_BYTES {
                return Err(WorkflowError::JobExecutionFailed(format!(
                    "ZIP entry '{}' exceeds the 500 MiB extraction limit",
                    enclosed.display()
                )));
            }
            written_total = written_total.saturating_add(written);
            if written_total > MAX_TOTAL_BYTES {
                return Err(WorkflowError::JobExecutionFailed(
                    "Source archive exceeds the 2 GiB extraction limit".to_string(),
                ));
            }
        }
        Ok(file_count)
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
        let target = self.target_path(&context);
        let archive_path = self.archive_path.clone();
        let extraction_target = target.clone();
        let extraction_permit = super::acquire_archive_extraction_permit().await?;
        let file_count = tokio::task::spawn_blocking(move || {
            let _extraction_permit = extraction_permit;
            fs::create_dir_all(&extraction_target).map_err(WorkflowError::IoError)?;
            Self::extract_archive(&archive_path, &extraction_target)
        })
        .await
        .map_err(|error| {
            WorkflowError::JobExecutionFailed(format!("Source extraction task failed: {error}"))
        })??;
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
        let target = self.target_path(context);
        match tokio::fs::remove_dir_all(&target).await {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(WorkflowError::IoError(error)),
        }
        Ok(())
    }

    fn cleanup_after_workflow(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn rejects_parent_directory_entries() {
        let error = PrepareSourceBundleJob::validate_path(Path::new("../secret"));
        assert!(matches!(
            error,
            Err(WorkflowError::InvalidArchiveEntry { .. })
        ));
    }

    #[test]
    fn rejects_sensitive_entries() {
        for path in [".env", "app/.env.local", ".git/config", "server.key"] {
            assert!(matches!(
                PrepareSourceBundleJob::validate_path(Path::new(path)),
                Err(WorkflowError::InvalidArchiveEntry { .. })
            ));
        }
    }

    #[test]
    fn extracts_regular_files_and_rejects_directory_only_archives_as_empty() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let archive_path = temporary.path().join("source.zip");
        let file = fs::File::create(&archive_path).expect("archive file");
        let mut archive = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        archive
            .add_directory("empty/", options)
            .expect("directory entry");
        archive
            .start_file("src/main.rs", options)
            .expect("file entry");
        archive.write_all(b"fn main() {}").expect("file content");
        archive.finish().expect("complete archive");

        let target = temporary.path().join("extracted");
        fs::create_dir_all(&target).expect("target");
        let count = PrepareSourceBundleJob::extract_archive(&archive_path, &target)
            .expect("archive should extract");

        assert_eq!(count, 1);
        assert_eq!(
            fs::read_to_string(target.join("src/main.rs")).expect("extracted file"),
            "fn main() {}"
        );
    }
}
