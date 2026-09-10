// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Print the generated Dockerfile for a given runtime preset to stdout.
//!
//! The `scripts/build-sandbox-images.sh` pipeline invokes this helper to
//! materialize the Dockerfile into a tempdir before handing it to
//! `docker buildx build --push`. Keeping the Dockerfile logic in Rust (and
//! driving the multi-arch buildx+push flow from bash) is simpler than
//! teaching an extra Rust example to speak buildx.
//!
//! Usage:
//!     cargo run -p temps-agents --example print_dockerfile -- <runtime>
//!
//! Valid runtimes: node, bun, python, rust, go, full.

use temps_agents::sandbox::docker::dockerfile_for_runtime;

const VALID_RUNTIMES: &[&str] = &["node", "bun", "python", "rust", "go", "full"];

fn main() {
    let runtime = match std::env::args().nth(1) {
        Some(r) => r,
        None => {
            eprintln!(
                "usage: print_dockerfile <runtime>\nvalid runtimes: {}",
                VALID_RUNTIMES.join(", ")
            );
            std::process::exit(2);
        }
    };

    if !VALID_RUNTIMES.contains(&runtime.as_str()) {
        eprintln!(
            "error: unknown runtime '{}'. valid: {}",
            runtime,
            VALID_RUNTIMES.join(", ")
        );
        std::process::exit(2);
    }

    print!("{}", dockerfile_for_runtime(&runtime));
}
