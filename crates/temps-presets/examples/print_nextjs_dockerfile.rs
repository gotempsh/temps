// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Print the Dockerfile the Next.js preset generates for a checkout.
//!
//! Usage:
//!     cargo run -p temps-presets --example print_nextjs_dockerfile -- \
//!         <repo-root> [app-dir] [project-slug]
//!
//! `app-dir` defaults to `repo-root`; pass the app's directory for a
//! monorepo subproject (e.g. `<repo-root>/apps/web`).

use std::path::PathBuf;
use temps_presets::{DockerfileConfig, NextJs, Preset};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut args = std::env::args().skip(1);
    let Some(root) = args.next().map(PathBuf::from) else {
        eprintln!("usage: print_nextjs_dockerfile <repo-root> [app-dir] [project-slug]");
        std::process::exit(2);
    };
    let app = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| root.clone());
    let slug = args.next().unwrap_or_else(|| "app".to_string());

    let dockerfile = NextJs
        .dockerfile(DockerfileConfig {
            root_local_path: &root,
            local_path: &app,
            install_command: None,
            build_command: None,
            output_dir: None,
            build_vars: None,
            project_slug: &slug,
            use_buildkit: true,
        })
        .await;
    print!("{}", dockerfile.content);
}
