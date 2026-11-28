// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where this process creates a directory that a sandbox will mount.
//!
//! An agent run's checkout, an autofix run's checkout, a provider smoke test
//! and a runtime probe each prepare a directory here and hand it to a sandbox
//! as its workspace. The Docker daemon mounts it, so it must be somewhere the
//! daemon can reach:
//!
//! * a native process uses its temporary directory, as always: the daemon
//!   shares its filesystem;
//! * a process that itself runs in a container uses its data directory. Its
//!   own temporary directory lives in the container's private filesystem,
//!   which no other container can mount; the data directory comes from a
//!   volume or host path that the daemon can (see
//!   [`temps_core::daemon_mount`]).

use std::path::{Path, PathBuf};

use temps_core::ExecutionEnvironment;

/// Directory under the data directory that holds sandbox workspaces prepared
/// by a containerized process.
pub const CONTAINERIZED_WORKSPACE_DIR: &str = "sandbox-workspaces";

/// [`sandbox_workspace_root`] for an explicit environment, temporary
/// directory and data directory.
pub fn sandbox_workspace_root_for(
    execution_environment: ExecutionEnvironment,
    temp_dir: &Path,
    data_dir: &Path,
) -> PathBuf {
    match execution_environment {
        ExecutionEnvironment::Host => temp_dir.to_path_buf(),
        ExecutionEnvironment::Docker => data_dir.join(CONTAINERIZED_WORKSPACE_DIR),
    }
}

/// The parent of every directory this process prepares for a sandbox to
/// mount. Callers create their own subdirectory; `create_dir_all` makes the
/// parent too.
pub fn sandbox_workspace_root() -> PathBuf {
    sandbox_workspace_root_for(
        temps_core::runtime::execution_environment_compatibility(),
        &std::env::temp_dir(),
        &crate::microsandbox::host_data_dir(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_native_process_keeps_using_its_temporary_directory() {
        assert_eq!(
            sandbox_workspace_root_for(
                ExecutionEnvironment::Host,
                Path::new("/tmp"),
                Path::new("/var/lib/temps")
            ),
            PathBuf::from("/tmp")
        );
    }

    #[test]
    fn a_containerized_process_prepares_workspaces_in_its_data_directory() {
        assert_eq!(
            sandbox_workspace_root_for(
                ExecutionEnvironment::Docker,
                Path::new("/tmp"),
                Path::new("/app/data")
            ),
            PathBuf::from("/app/data/sandbox-workspaces")
        );
    }
}
