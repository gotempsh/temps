// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Which entries of an uploaded project archive (Drop) are kept, left out, or
//! refused.
//!
//! One rule for every place that reads a Drop archive (the inspection that
//! detects the project, and the extraction the build uses), and the same rule
//! the console applies to a dropped folder, so a project behaves the same
//! whether it arrives as a folder or as a ZIP:
//!
//! - **Kept**: everything else, including environment *templates*
//!   (`.env.example`, `.env.sample`, `.env.template`, `.env.dist`), which
//!   projects commit on purpose and builds often read.
//! - **Left out** and reported: version control (`.git/`), installed
//!   dependencies (`node_modules/`), real environment files (`.env`,
//!   `.env.local`, `.env.production`, ...), which may hold secrets and are set
//!   in the environment's settings instead, and OS metadata (`__MACOSX/`,
//!   `.DS_Store`, `Thumbs.db`).
//! - **Refused**: private material that has no business in an upload
//!   (`*.pem`, `*.key`, `credentials.json`). The whole archive is rejected and
//!   the entry named, because silently dropping a key could break the build
//!   in a way nobody would trace back to it.

use std::collections::BTreeMap;
use std::path::{Component, Path};

/// Last dot-separated suffixes that make a `.env.*` file a template.
pub const ENV_TEMPLATE_SUFFIXES: &[&str] = &["example", "sample", "template", "dist"];

/// Distinct left-out paths described individually in a [`DropSkipReport`].
pub const MAX_REPORTED_SKIPPED_PATHS: usize = 50;

/// Why an entry was left out of a Drop upload.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DropSkipReason {
    /// `.git/`: repository history, not part of the build.
    VersionControl,
    /// `node_modules/`: installed dependencies; the build installs its own.
    Dependencies,
    /// `.env`, `.env.local`, ...: may hold secrets. Set the variables in the
    /// environment's settings instead.
    EnvironmentFile,
    /// `__MACOSX/`, `.DS_Store`, `Thumbs.db`: written by the OS when zipping.
    OsMetadata,
}

impl std::fmt::Display for DropSkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            DropSkipReason::VersionControl => "version control history",
            DropSkipReason::Dependencies => "installed dependencies (the build installs them)",
            DropSkipReason::EnvironmentFile => {
                "environment file that may hold secrets (set variables in the environment settings)"
            }
            DropSkipReason::OsMetadata => "operating system metadata",
        })
    }
}

/// Why an archive is refused because of one of its entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropRefusalReason {
    /// `*.pem` / `*.key`.
    PrivateKey,
    /// `credentials.json`.
    Credentials,
}

impl std::fmt::Display for DropRefusalReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            DropRefusalReason::PrivateKey => "looks like a private key or certificate",
            DropRefusalReason::Credentials => "looks like a credentials file",
        })
    }
}

/// What to do with one archive entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropEntryDisposition {
    Keep,
    /// Leave it out. `reported_path` is what the notice lists: the directory
    /// (`app/node_modules/`) for directory rules, so one dependency tree is
    /// one line rather than thousands, and the file itself otherwise.
    Skip {
        reason: DropSkipReason,
        reported_path: String,
    },
    /// Reject the whole archive, naming this entry.
    Refuse {
        reason: DropRefusalReason,
    },
}

/// Whether `name` is a `.env` file other than a template.
fn is_environment_file(name: &str) -> bool {
    if name == ".env" {
        return true;
    }
    let Some(rest) = name.strip_prefix(".env.") else {
        return false;
    };
    let suffix = rest.rsplit('.').next().unwrap_or(rest);
    !ENV_TEMPLATE_SUFFIXES.contains(&suffix)
}

/// Classify an archive entry by its (already sanitized, relative) path.
///
/// Directory rules win over file rules, so a test certificate inside
/// `node_modules/` leaves the dependency tree out instead of refusing the
/// upload.
pub fn classify_drop_entry(path: &Path) -> DropEntryDisposition {
    let names: Vec<String> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    let Some(last) = names.len().checked_sub(1) else {
        return DropEntryDisposition::Keep;
    };
    let prefix_through = |index: usize| {
        let mut prefix = names[..=index].join("/");
        if index < last {
            prefix.push('/');
        }
        prefix
    };

    for (index, name) in names.iter().enumerate() {
        let directory_reason = match name.as_str() {
            ".git" => Some(DropSkipReason::VersionControl),
            "node_modules" => Some(DropSkipReason::Dependencies),
            "__MACOSX" => Some(DropSkipReason::OsMetadata),
            _ => None,
        };
        if let Some(reason) = directory_reason {
            return DropEntryDisposition::Skip {
                reason,
                reported_path: prefix_through(index),
            };
        }
    }
    for (index, name) in names.iter().enumerate() {
        if is_environment_file(name) {
            return DropEntryDisposition::Skip {
                reason: DropSkipReason::EnvironmentFile,
                reported_path: prefix_through(index),
            };
        }
    }
    if matches!(names[last].as_str(), ".DS_Store" | "Thumbs.db") {
        return DropEntryDisposition::Skip {
            reason: DropSkipReason::OsMetadata,
            reported_path: prefix_through(last),
        };
    }
    for name in &names {
        if name.ends_with(".pem") || name.ends_with(".key") {
            return DropEntryDisposition::Refuse {
                reason: DropRefusalReason::PrivateKey,
            };
        }
        if name == "credentials.json" {
            return DropEntryDisposition::Refuse {
                reason: DropRefusalReason::Credentials,
            };
        }
    }
    DropEntryDisposition::Keep
}

/// One line of the "left out of the upload" notice.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct DropSkippedPath {
    /// The file, or the directory (ending in `/`) whose files were left out.
    pub path: String,
    pub reason: DropSkipReason,
    /// Archive entries left out under this path.
    pub files: u64,
}

/// Bounded record of what a Drop upload left out: at most
/// [`MAX_REPORTED_SKIPPED_PATHS`] distinct paths, plus totals.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DropSkipReport {
    paths: BTreeMap<String, (DropSkipReason, u64)>,
    /// Entries left out under paths beyond the reporting limit.
    unreported_files: u64,
    total_files: u64,
}

impl DropSkipReport {
    pub fn record(&mut self, reason: DropSkipReason, reported_path: String) {
        self.total_files += 1;
        if let Some((_, files)) = self.paths.get_mut(&reported_path) {
            *files += 1;
        } else if self.paths.len() < MAX_REPORTED_SKIPPED_PATHS {
            self.paths.insert(reported_path, (reason, 1));
        } else {
            self.unreported_files += 1;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.total_files == 0
    }

    /// Entries left out in total.
    pub fn total_files(&self) -> u64 {
        self.total_files
    }

    /// Entries left out under paths not listed by [`Self::paths`].
    pub fn unreported_files(&self) -> u64 {
        self.unreported_files
    }

    /// The listed paths, sorted.
    pub fn paths(&self) -> Vec<DropSkippedPath> {
        self.paths
            .iter()
            .map(|(path, (reason, files))| DropSkippedPath {
                path: path.clone(),
                reason: *reason,
                files: *files,
            })
            .collect()
    }

    /// `node_modules/ (1200 files), .env.local` for a log line.
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = self
            .paths
            .iter()
            .map(|(path, (_, files))| {
                if *files > 1 {
                    format!("{path} ({files} files)")
                } else {
                    path.clone()
                }
            })
            .collect();
        if self.unreported_files > 0 {
            parts.push(format!("{} more file(s)", self.unreported_files));
        }
        parts.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(path: &str) -> DropEntryDisposition {
        classify_drop_entry(Path::new(path))
    }

    fn skipped(path: &str) -> (DropSkipReason, String) {
        match classify(path) {
            DropEntryDisposition::Skip {
                reason,
                reported_path,
            } => (reason, reported_path),
            other => panic!("{path} should be skipped, got {other:?}"),
        }
    }

    #[test]
    fn environment_templates_are_kept() {
        for path in [
            ".env.example",
            ".env.sample",
            ".env.template",
            ".env.dist",
            "apps/api/.env.production.example",
            "Cargo.toml",
            "src/main.rs",
            ".envrc.d/notes",
        ] {
            assert_eq!(classify(path), DropEntryDisposition::Keep, "{path}");
        }
    }

    #[test]
    fn real_environment_files_are_left_out_not_refused() {
        for path in [
            ".env",
            ".env.local",
            "app/.env.production",
            ".env.development.local",
        ] {
            let (reason, reported) = skipped(path);
            assert_eq!(reason, DropSkipReason::EnvironmentFile, "{path}");
            assert_eq!(reported, path);
        }
        // A `.env/` virtualenv directory is left out as a whole.
        assert_eq!(
            skipped(".env/bin/python"),
            (DropSkipReason::EnvironmentFile, ".env/".to_string())
        );
    }

    #[test]
    fn generated_directories_are_left_out_as_one_line() {
        assert_eq!(
            skipped("node_modules/react/index.js"),
            (DropSkipReason::Dependencies, "node_modules/".to_string())
        );
        assert_eq!(
            skipped("web/node_modules/a/b.js"),
            (
                DropSkipReason::Dependencies,
                "web/node_modules/".to_string()
            )
        );
        assert_eq!(
            skipped(".git/config"),
            (DropSkipReason::VersionControl, ".git/".to_string())
        );
        assert_eq!(
            skipped("__MACOSX/._main.rs"),
            (DropSkipReason::OsMetadata, "__MACOSX/".to_string())
        );
        assert_eq!(
            skipped("src/.DS_Store"),
            (DropSkipReason::OsMetadata, "src/.DS_Store".to_string())
        );
    }

    #[test]
    fn private_material_is_refused() {
        for (path, expected) in [
            ("server.key", DropRefusalReason::PrivateKey),
            ("certs/tls.pem", DropRefusalReason::PrivateKey),
            ("config/credentials.json", DropRefusalReason::Credentials),
        ] {
            assert_eq!(
                classify(path),
                DropEntryDisposition::Refuse { reason: expected },
                "{path}"
            );
        }
    }

    #[test]
    fn a_left_out_directory_wins_over_a_key_inside_it() {
        assert_eq!(
            skipped("node_modules/some-lib/test/fixture.pem"),
            (DropSkipReason::Dependencies, "node_modules/".to_string())
        );
    }

    #[test]
    fn the_skip_report_collapses_directories_and_stays_bounded() {
        let mut report = DropSkipReport::default();
        for file in 0..3 {
            let path = format!("node_modules/pkg/{file}.js");
            if let DropEntryDisposition::Skip {
                reason,
                reported_path,
            } = classify_drop_entry(Path::new(&path))
            {
                report.record(reason, reported_path);
            }
        }
        report.record(DropSkipReason::EnvironmentFile, ".env".to_string());
        assert_eq!(report.total_files(), 4);
        assert_eq!(report.summary(), ".env, node_modules/ (3 files)");

        for index in 0..(MAX_REPORTED_SKIPPED_PATHS + 5) {
            report.record(DropSkipReason::EnvironmentFile, format!("a{index}/.env"));
        }
        assert_eq!(report.paths().len(), MAX_REPORTED_SKIPPED_PATHS);
        assert_eq!(report.unreported_files(), 7);
        assert!(report.summary().ends_with("7 more file(s)"));
    }
}
