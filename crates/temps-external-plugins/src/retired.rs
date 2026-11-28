// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Plugins an earlier version installed as executables.
//!
//! A plugin runs only as a Temps service. An executable installed before
//! that is never started again: it is listed, with the reason, until an
//! administrator removes it. Listing reads the installation's own record and
//! runs nothing, so it needs none of the verification that starting did.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The record an installation's directory holds while it is selected.
const ACTIVE_FILE: &str = "active.json";
/// An active record is a few short fields; anything larger is not one.
const MAX_ACTIVE_RECORD_BYTES: u64 = 64 * 1024;
/// How many installation directories are read. An instance has a handful.
const MAX_INSTALLATIONS: usize = 512;

/// What an administrator is told about a plugin that no longer runs.
pub const RETIRED_REASON: &str = "This plugin was installed as an executable, and plugins now run \
    only as Temps services. Deploy the plugin as a service, remove this entry, then add the \
    service. The files it stored on this server are kept and are not moved.";

/// A plugin installed as an executable, which is no longer started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetiredPlugin {
    pub name: String,
    /// The version that was active, when its record still says.
    pub version: Option<String>,
}

#[derive(Deserialize)]
struct ActiveRecord {
    version: String,
}

/// The executables installed under `plugins_dir`, by name.
///
/// A directory counts when its name is a plugin name and it holds an active
/// record that is a regular file. A missing or unreadable directory lists
/// nothing: there is then nothing an administrator could act on.
pub async fn installed_executables(plugins_dir: &Path) -> Vec<RetiredPlugin> {
    let mut entries = match tokio::fs::read_dir(plugins_dir).await {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    let mut retired = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        if retired.len() >= MAX_INSTALLATIONS {
            break;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !crate::naming::valid_plugin_name(&name) {
            continue;
        }
        // A link in place of the directory is not an installation: removal
        // refuses it, so it is not listed either.
        if !matches!(entry.file_type().await, Ok(kind) if kind.is_dir()) {
            continue;
        }
        let active = entry.path().join(ACTIVE_FILE);
        // Never follow a link out of the installation directory.
        let is_record = matches!(
            tokio::fs::symlink_metadata(&active).await,
            Ok(metadata) if metadata.is_file() && metadata.len() <= MAX_ACTIVE_RECORD_BYTES
        );
        if !is_record {
            continue;
        }
        let version = tokio::fs::read(&active)
            .await
            .ok()
            .and_then(|bytes| serde_json::from_slice::<ActiveRecord>(&bytes).ok())
            .map(|record| record.version)
            .filter(|version| crate::naming::valid_version(version));
        retired.push(RetiredPlugin { name, version });
    }
    retired.sort_by(|left, right| left.name.cmp(&right.name));
    retired
}

/// The record that lists `name` as an installed executable, when it is one:
/// a regular file in a real directory under `plugins_dir`, never a link.
async fn record_of(plugins_dir: &Path, name: &str) -> Result<Option<PathBuf>, std::io::Error> {
    if !crate::naming::valid_plugin_name(name) {
        return Ok(None);
    }
    let directory = plugins_dir.join(name);
    let active = directory.join(ACTIVE_FILE);
    for (path, is_expected) in [
        (
            &directory,
            std::fs::Metadata::is_dir as fn(&std::fs::Metadata) -> bool,
        ),
        (&active, std::fs::Metadata::is_file),
    ] {
        match tokio::fs::symlink_metadata(path).await {
            Ok(metadata) if is_expected(&metadata) => {}
            Ok(_) => return Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
    }
    Ok(Some(active))
}

/// Whether `name` is listed as an installed executable.
pub async fn is_installed(plugins_dir: &Path, name: &str) -> Result<bool, std::io::Error> {
    Ok(record_of(plugins_dir, name).await?.is_some())
}

/// Remove the record that lists `name` as an installed executable.
///
/// Only the record is removed: the installation's releases and the files the
/// plugin stored stay where they are. Returns whether there was a record.
/// Neither the installation directory nor the record may be a link; this
/// never removes anything outside `plugins_dir`.
pub async fn remove(plugins_dir: &Path, name: &str) -> Result<bool, std::io::Error> {
    let Some(active) = record_of(plugins_dir, name).await? else {
        return Ok(false);
    };
    tokio::fs::remove_file(&active).await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install(root: &Path, name: &str, record: &str) {
        let directory = root.join(name);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join(ACTIVE_FILE), record).unwrap();
    }

    #[tokio::test]
    async fn lists_each_selected_installation_with_its_version() {
        let root = tempfile::tempdir().unwrap();
        install(
            root.path(),
            "reports",
            r#"{"version":"1.4.0","directory":"1.4.0-abc"}"#,
        );
        install(
            root.path(),
            "audit-trail",
            r#"{"version":"0.2.1","directory":"0.2.1-def","isolation":"container"}"#,
        );

        assert_eq!(
            installed_executables(root.path()).await,
            vec![
                RetiredPlugin {
                    name: "audit-trail".into(),
                    version: Some("0.2.1".into()),
                },
                RetiredPlugin {
                    name: "reports".into(),
                    version: Some("1.4.0".into()),
                },
            ]
        );
    }

    #[tokio::test]
    async fn a_removed_or_never_selected_installation_is_not_listed() {
        let root = tempfile::tempdir().unwrap();
        // Releases on disk, but no active record: it was removed.
        std::fs::create_dir_all(root.path().join("reports").join("1.4.0-abc")).unwrap();
        // Not a plugin name.
        install(root.path(), "Not_A_Plugin", r#"{"version":"1.0.0"}"#);
        // A stray file next to the installations.
        std::fs::write(root.path().join("registry-state.json"), "{}").unwrap();

        assert_eq!(installed_executables(root.path()).await, Vec::new());
    }

    #[tokio::test]
    async fn an_unreadable_record_still_lists_the_plugin_without_a_version() {
        let root = tempfile::tempdir().unwrap();
        install(root.path(), "reports", "not json");
        install(root.path(), "exports", r#"{"version":"../../etc"}"#);

        assert_eq!(
            installed_executables(root.path()).await,
            vec![
                RetiredPlugin {
                    name: "exports".into(),
                    version: None,
                },
                RetiredPlugin {
                    name: "reports".into(),
                    version: None,
                },
            ]
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_record_that_is_a_link_is_not_read() {
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside.json");
        std::fs::write(&outside, r#"{"version":"9.9.9"}"#).unwrap();
        let installations = root.path().join("plugins");
        std::fs::create_dir_all(installations.join("reports")).unwrap();
        std::os::unix::fs::symlink(&outside, installations.join("reports").join(ACTIVE_FILE))
            .unwrap();

        assert_eq!(installed_executables(&installations).await, Vec::new());
    }

    #[tokio::test]
    async fn removing_takes_the_plugin_off_the_list_and_keeps_its_files() {
        let root = tempfile::tempdir().unwrap();
        install(root.path(), "reports", r#"{"version":"1.4.0"}"#);
        let release = root.path().join("reports").join("1.4.0-abc");
        std::fs::create_dir_all(&release).unwrap();
        std::fs::write(release.join("plugin"), "binary").unwrap();

        assert!(is_installed(root.path(), "reports").await.unwrap());
        assert!(remove(root.path(), "reports").await.unwrap());

        assert!(!is_installed(root.path(), "reports").await.unwrap());
        assert_eq!(installed_executables(root.path()).await, Vec::new());
        assert!(release.join("plugin").is_file());
        // Nothing is left to remove a second time, and nothing is invented.
        assert!(!remove(root.path(), "reports").await.unwrap());
        assert!(!remove(root.path(), "never-installed").await.unwrap());
        assert!(!remove(root.path(), "../reports").await.unwrap());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn removing_never_follows_a_link_out_of_the_installations() {
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join(ACTIVE_FILE), r#"{"version":"1.0.0"}"#).unwrap();
        let installations = root.path().join("plugins");
        std::fs::create_dir_all(&installations).unwrap();
        std::os::unix::fs::symlink(&outside, installations.join("reports")).unwrap();

        // What cannot be removed is not listed either.
        assert_eq!(installed_executables(&installations).await, Vec::new());
        assert!(!is_installed(&installations, "reports").await.unwrap());
        assert!(!remove(&installations, "reports").await.unwrap());
        assert!(outside.join(ACTIVE_FILE).is_file());
    }

    #[tokio::test]
    async fn no_installation_directory_lists_nothing() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            installed_executables(&root.path().join("missing")).await,
            Vec::new()
        );
    }

    #[test]
    fn the_reason_says_what_to_do_and_what_happens_to_the_data() {
        assert!(RETIRED_REASON.contains("run only as Temps services"));
        assert!(RETIRED_REASON.contains("Deploy the plugin as a service"));
        assert!(RETIRED_REASON.contains("remove this entry"));
        assert!(RETIRED_REASON.contains("are kept"));
    }
}
