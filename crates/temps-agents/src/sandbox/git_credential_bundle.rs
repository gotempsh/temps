//! Files for the in-sandbox `temps-git-credential-helper` and
//! `temps-git-credential-daemon` binaries, embedded at compile time.
//! The Dockerfile's `git-credential-builder` stage compiles from these
//! sources — no separate publish step required.
//!
//! Each file ships with the path it must land at in the build-context
//! tar sent to `docker build`. The Dockerfile references those paths
//! verbatim under the `git-credential/` directory.

use super::pty_agent_bundle::BundleFile;

pub const BUNDLE: &[BundleFile] = &[
    BundleFile {
        path: "git-credential/Cargo.toml",
        contents: include_bytes!("../../../temps-git-credential/Cargo.docker.toml"),
        mode: 0o644,
    },
    BundleFile {
        path: "git-credential/src/lib.rs",
        contents: include_bytes!("../../../temps-git-credential/src/lib.rs"),
        mode: 0o644,
    },
    BundleFile {
        path: "git-credential/src/helper_protocol.rs",
        contents: include_bytes!("../../../temps-git-credential/src/helper_protocol.rs"),
        mode: 0o644,
    },
    BundleFile {
        path: "git-credential/src/ipc.rs",
        contents: include_bytes!("../../../temps-git-credential/src/ipc.rs"),
        mode: 0o644,
    },
    BundleFile {
        path: "git-credential/src/bin/helper.rs",
        contents: include_bytes!("../../../temps-git-credential/src/bin/helper.rs"),
        mode: 0o644,
    },
    BundleFile {
        path: "git-credential/src/bin/daemon.rs",
        contents: include_bytes!("../../../temps-git-credential/src/bin/daemon.rs"),
        mode: 0o644,
    },
];

/// Append every [`BUNDLE`] entry to an in-progress tar builder. Mirrors
/// the [`super::pty_agent_bundle::append_to_tar`] pattern.
pub fn append_to_tar<W: std::io::Write>(
    tar_builder: &mut tar::Builder<W>,
) -> Result<(), std::io::Error> {
    for file in BUNDLE {
        let mut header = tar::Header::new_gnu();
        header.set_size(file.contents.len() as u64);
        header.set_path(file.path)?;
        header.set_mode(file.mode);
        header.set_cksum();
        tar_builder.append(&header, file.contents)?;
    }
    Ok(())
}
