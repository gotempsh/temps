// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Records which commit this guest agent was built from, so the host can tell
//! when the agent it stages has fallen behind the `temps` binary. Falls back
//! to `unknown` when there is no git checkout (a source tarball, a container
//! build context), which the host treats as "cannot tell", never as stale.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods,
    reason = "build script: panicking is how a build script reports failure, and it reads the variables Cargo sets for it"
)]
use std::process::Command;

fn main() {
    let commit = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=TEMPS_AGENT_BUILD_COMMIT={commit}");
}
