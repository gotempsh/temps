// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Files for the in-sandbox `temps-pty-agent` binary, embedded at compile
//! time. The Dockerfile's pty-agent builder stage compiles from these
//! sources, so bumping the agent requires only a recompile of `temps`
//! itself — no separate publish step.
//!
//! Each file ships with the path it must land at in the build context tar
//! sent to `docker build`. The Dockerfile references those paths verbatim.

/// One file to pack into the build-context tar.
pub struct BundleFile {
    /// Path inside the build context (relative, no leading slash).
    pub path: &'static str,
    /// File contents.
    pub contents: &'static [u8],
    /// Unix mode bits. 0o644 for sources, 0o755 for scripts.
    pub mode: u32,
}

/// All files needed to build and run the agent inside the sandbox image.
/// Keep the `path` values in sync with the `COPY` instructions emitted by
/// [`super::docker::dockerfile_for_runtime`].
pub const BUNDLE: &[BundleFile] = &[
    BundleFile {
        path: "pty-agent/Cargo.toml",
        contents: include_bytes!("../../../temps-pty-agent/Cargo.docker.toml"),
        mode: 0o644,
    },
    BundleFile {
        path: "pty-agent/src/main.rs",
        contents: include_bytes!("../../../temps-pty-agent/src/main.rs"),
        mode: 0o644,
    },
    BundleFile {
        path: "pty-agent/src/lib.rs",
        contents: include_bytes!("../../../temps-pty-agent/src/lib.rs"),
        mode: 0o644,
    },
    BundleFile {
        path: "pty-agent/src/protocol.rs",
        contents: include_bytes!("../../../temps-pty-agent/src/protocol.rs"),
        mode: 0o644,
    },
    BundleFile {
        path: "pty-agent/src/pty.rs",
        contents: include_bytes!("../../../temps-pty-agent/src/pty.rs"),
        mode: 0o644,
    },
    BundleFile {
        path: "pty-agent/src/server.rs",
        contents: include_bytes!("../../../temps-pty-agent/src/server.rs"),
        mode: 0o644,
    },
    BundleFile {
        path: "pty-agent/sandbox-entrypoint.sh",
        contents: include_bytes!("../../../temps-pty-agent/docker/sandbox-entrypoint.sh"),
        mode: 0o755,
    },
];

/// Append every [`BUNDLE`] entry to an in-progress tar builder. The caller
/// is responsible for also writing the Dockerfile and finishing the tar.
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
