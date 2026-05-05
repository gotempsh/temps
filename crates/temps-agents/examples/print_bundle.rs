//! Materialize the sandbox image build-context bundles (pty-agent +
//! git-credential) into a target directory.
//!
//! The CI release workflow and `scripts/build-sandbox-images.sh` use this
//! together with `print_dockerfile` to prepare a real on-disk build context
//! for `docker buildx build --push`. At runtime, the temps server streams
//! the same files via an in-memory tar (see `sandbox::docker::build_context_tar`).
//! Both bundles must be materialized — the generated Dockerfile has a
//! `COPY pty-agent/` AND a `COPY git-credential/` stage, so omitting either
//! breaks the build.
//!
//! Usage:
//!     cargo run -p temps-agents --example print_bundle -- <target-dir>
//!
//! The directory is created if it doesn't exist. Existing files are
//! overwritten.

use std::io::Write;
use std::path::{Path, PathBuf};

use temps_agents::sandbox::git_credential_bundle::BUNDLE as GIT_CREDENTIAL_BUNDLE;
use temps_agents::sandbox::pty_agent_bundle::{BundleFile, BUNDLE as PTY_AGENT_BUNDLE};

fn main() {
    let target = match std::env::args().nth(1) {
        Some(t) => PathBuf::from(t),
        None => {
            eprintln!("usage: print_bundle <target-dir>");
            std::process::exit(2);
        }
    };

    if let Err(e) = std::fs::create_dir_all(&target) {
        eprintln!("error: create {}: {}", target.display(), e);
        std::process::exit(1);
    }

    materialize(&target, PTY_AGENT_BUNDLE);
    materialize(&target, GIT_CREDENTIAL_BUNDLE);
}

fn materialize(target: &Path, bundle: &[BundleFile]) {
    for file in bundle {
        let dest = target.join(file.path);
        if let Some(parent) = dest.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!("error: create {}: {}", parent.display(), e);
                std::process::exit(1);
            }
        }

        let mut out = match std::fs::File::create(&dest) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("error: create {}: {}", dest.display(), e);
                std::process::exit(1);
            }
        };
        if let Err(e) = out.write_all(file.contents) {
            eprintln!("error: write {}: {}", dest.display(), e);
            std::process::exit(1);
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(file.mode);
            if let Err(e) = std::fs::set_permissions(&dest, perms) {
                eprintln!("error: chmod {}: {}", dest.display(), e);
                std::process::exit(1);
            }
        }

        println!("{}", dest.display());
    }
}
