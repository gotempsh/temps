// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Reads `.temps.yaml` from the source tree a deployment produced.
//!
//! The same reader serves a git checkout (`download_repo`) and an uploaded
//! bundle (`prepare_source_bundle`): both leave the project's files under a
//! `repo_dir` output, so the post-deploy configuration jobs (cron jobs, metric
//! alert rules) do not care where the tree came from.

use std::path::{Path, PathBuf};
use temps_core::TempsConfig;
use thiserror::Error;

use super::build_image::read_confined_control_file;

/// Upper bound for `.temps.yaml`; far above any real file, small enough that a
/// hostile bundle cannot make a deploy read an unbounded amount of memory.
const MAX_TEMPS_YAML_BYTES: u64 = 1024 * 1024;

/// A `.temps.yaml` that exists but cannot be used.
///
/// A missing file is not an error (see [`TempsYamlLookup::Missing`]); these are
/// the cases a user must be told about, because the deployment carries on
/// without the configuration they wrote.
#[derive(Debug, Error)]
pub enum TempsYamlError {
    #[error("Cannot read .temps.yaml at '{path}': {reason}")]
    Unreadable { path: String, reason: String },

    #[error("Cannot parse .temps.yaml at '{path}': {reason}")]
    Invalid { path: String, reason: String },
}

/// Result of looking for `.temps.yaml` in a source tree.
#[derive(Debug)]
pub enum TempsYamlLookup {
    /// The source tree has no `.temps.yaml`; nothing to configure.
    Missing { path: PathBuf },
    Found {
        path: PathBuf,
        config: Box<TempsConfig>,
    },
}

/// Locate and parse `<repo_dir>/<directory>/.temps.yaml`.
///
/// `directory` is the project's configured root directory. A leading `/` is
/// ignored so `"/"` means the tree root, not the host filesystem root.
pub fn read_temps_yaml(
    repo_dir: &Path,
    directory: &str,
) -> Result<TempsYamlLookup, TempsYamlError> {
    let relative = directory.trim_start_matches('/');
    let path = repo_dir.join(relative).join(".temps.yaml");

    let contents = match read_confined_control_file(repo_dir, &path, MAX_TEMPS_YAML_BYTES) {
        Ok(Some(contents)) => contents,
        Ok(None) => return Ok(TempsYamlLookup::Missing { path }),
        Err(error) => {
            return Err(TempsYamlError::Unreadable {
                path: path.display().to_string(),
                reason: format!(
                    "{error}. .temps.yaml must be a regular file inside the project source, not a symlink or a file outside it"
                ),
            })
        }
    };

    let config = TempsConfig::from_yaml(&contents).map_err(|error| TempsYamlError::Invalid {
        path: path.display().to_string(),
        reason: error.to_string(),
    })?;
    Ok(TempsYamlLookup::Found {
        path,
        config: Box::new(config),
    })
}

/// Fixtures shared by the jobs that consume [`read_temps_yaml`].
#[cfg(test)]
pub(crate) mod test_support {
    use chrono::Utc;
    use temps_entities::projects;

    /// A Drop-style project: no git repository, root directory `.`.
    pub(crate) fn uploaded_source_project(id: i32) -> projects::Model {
        let now = Utc::now();
        projects::Model {
            id,
            name: "drop-app".into(),
            repo_name: String::new(),
            repo_owner: String::new(),
            directory: ".".into(),
            pull_only_root_directory: false,
            main_branch: "main".into(),
            preset: temps_entities::preset::Preset::Static,
            preset_config: None,
            deployment_config: None,
            created_at: now,
            updated_at: now,
            slug: "drop-app".into(),
            is_deleted: false,
            deleted_at: None,
            last_deployment: None,
            is_public_repo: false,
            git_url: None,
            git_provider_connection_id: None,
            attack_mode: false,
            ai_alert_summaries_enabled: None,
            ai_api_traffic_summary_enabled: None,
            allow_alternate_sources: None,
            ai_debug_chat_enabled: None,
            ai_write_actions_enabled: false,
            error_source_context_enabled: false,
            vulnerability_scanning_enabled: false,
            error_source_root: None,
            enable_preview_environments: false,
            preview_envs_on_demand: false,
            preview_envs_idle_timeout_seconds: 0,
            preview_envs_wake_timeout_seconds: 0,
            source_type: temps_entities::source_type::SourceType::UploadedSource,
            project_type: temps_entities::types::ProjectType::Server,
            template_slug: None,
            service_template: None,
            hosting: Default::default(),
            instantiation_token: None,
            gitlab_webhook_id: None,
            gitlab_webhook_signing_token: None,
            gitea_webhook_signing_token: None,
            github_webhook_id: None,
            github_webhook_signing_token: None,
            bitbucket_webhook_token: None,
            bitbucket_webhook_hook_id: None,
            generic_webhook_token: None,
            cross_project_trace_sharing: false,
            image_retention_hours: None,
            cloud_telemetry_fidelity:
                temps_entities::cloud_telemetry_fidelity::CloudTelemetryFidelity::Metered,
            cloud_telemetry_write_mode:
                temps_entities::cloud_telemetry_write_mode::CloudTelemetryWriteMode::Local,
            cloud_analytics_write_mode:
                temps_entities::cloud_analytics_write_mode::CloudAnalyticsWriteMode::Local,
            cloud_telemetry_attribute_allowlist: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn found(lookup: TempsYamlLookup) -> TempsConfig {
        match lookup {
            TempsYamlLookup::Found { config, .. } => *config,
            TempsYamlLookup::Missing { path } => panic!("expected a config at {path:?}"),
        }
    }

    #[test]
    fn reads_cron_entries_from_the_tree_root() {
        let tree = tempfile::tempdir().expect("tempdir");
        fs::write(
            tree.path().join(".temps.yaml"),
            "cron:\n  - path: /api/cleanup\n    schedule: \"*/5 * * * *\"\n",
        )
        .expect("write");

        let config = found(read_temps_yaml(tree.path(), ".").expect("reads"));

        assert!(config.has_crons());
        let jobs = config.cron_jobs();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].path, "/api/cleanup");
        assert_eq!(jobs[0].schedule, "*/5 * * * *");
    }

    #[test]
    fn a_root_directory_of_slash_stays_inside_the_tree() {
        let tree = tempfile::tempdir().expect("tempdir");
        fs::write(
            tree.path().join(".temps.yaml"),
            "cron:\n  - path: /a\n    schedule: \"0 * * * *\"\n",
        )
        .expect("write");

        assert!(found(read_temps_yaml(tree.path(), "/").expect("reads")).has_crons());
    }

    #[test]
    fn honours_a_nested_project_directory() {
        let tree = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(tree.path().join("apps/web")).expect("mkdir");
        fs::write(
            tree.path().join("apps/web/.temps.yaml"),
            "cron:\n  - path: /nested\n    schedule: \"0 0 * * *\"\n",
        )
        .expect("write");

        let config = found(read_temps_yaml(tree.path(), "apps/web").expect("reads"));
        assert_eq!(config.cron_jobs()[0].path, "/nested");
        // The tree root has no file of its own.
        assert!(matches!(
            read_temps_yaml(tree.path(), ".").expect("lookup"),
            TempsYamlLookup::Missing { .. }
        ));
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let tree = tempfile::tempdir().expect("tempdir");
        fs::write(tree.path().join("index.html"), "<!doctype html>").expect("write");

        assert!(matches!(
            read_temps_yaml(tree.path(), ".").expect("lookup"),
            TempsYamlLookup::Missing { .. }
        ));
    }

    #[test]
    fn a_file_without_crons_parses_but_has_none() {
        let tree = tempfile::tempdir().expect("tempdir");
        fs::write(tree.path().join(".temps.yaml"), "{}\n").expect("write");

        assert!(!found(read_temps_yaml(tree.path(), ".").expect("reads")).has_crons());
    }

    #[test]
    fn unparsable_yaml_names_the_file_and_the_cause() {
        let tree = tempfile::tempdir().expect("tempdir");
        fs::write(tree.path().join(".temps.yaml"), "cron: [unclosed\n  - : :").expect("write");

        match read_temps_yaml(tree.path(), ".") {
            Err(TempsYamlError::Invalid { path, reason }) => {
                assert!(path.ends_with(".temps.yaml"), "{path}");
                assert!(!reason.is_empty());
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn a_directory_named_temps_yaml_is_unreadable_not_missing() {
        let tree = tempfile::tempdir().expect("tempdir");
        fs::create_dir(tree.path().join(".temps.yaml")).expect("mkdir");

        assert!(matches!(
            read_temps_yaml(tree.path(), "."),
            Err(TempsYamlError::Unreadable { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_file_is_refused_rather_than_followed() {
        let tree = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("tempdir");
        fs::write(
            outside.path().join("elsewhere.yaml"),
            "cron:\n  - path: /x\n    schedule: \"0 * * * *\"\n",
        )
        .expect("write");
        std::os::unix::fs::symlink(
            outside.path().join("elsewhere.yaml"),
            tree.path().join(".temps.yaml"),
        )
        .expect("symlink");

        assert!(matches!(
            read_temps_yaml(tree.path(), "."),
            Err(TempsYamlError::Unreadable { .. })
        ));
    }
}
