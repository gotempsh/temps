// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::env;
use std::path::Path;
use std::process::Command;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let web_dir = Path::new(&manifest_dir).join("web");
    let dist_dir = web_dir.join("dist");

    // Rerun if any web source changes or env var changes
    println!("cargo:rerun-if-changed=web/src");
    println!("cargo:rerun-if-changed=web/index.html");
    println!("cargo:rerun-if-changed=web/vite.config.ts");
    println!("cargo:rerun-if-changed=web/package.json");
    println!("cargo:rerun-if-env-changed=FORCE_WEB_BUILD");

    // In debug mode, skip building the web UI unless FORCE_WEB_BUILD is set.
    // Developers can run `bun run dev` in web/ for hot-reload during development.
    let profile = env::var("PROFILE").unwrap_or_default();
    if profile == "debug" && env::var("FORCE_WEB_BUILD").is_err() {
        println!("cargo:warning=Skipping plugin web build in debug mode (use FORCE_WEB_BUILD=1 to build)");
        // Create an empty dist/ so include_dir! doesn't fail
        let _ = std::fs::create_dir_all(&dist_dir);
        // Write a minimal index.html fallback for dev
        let fallback = dist_dir.join("index.html");
        if !fallback.exists() {
            let _ = std::fs::write(
                &fallback,
                r#"<!DOCTYPE html>
<html>
<head><meta charset="utf-8"><title>SEO Analyzer (dev)</title></head>
<body style="font-family:system-ui;padding:2rem;color:#a1a1aa;background:#09090b;text-align:center">
<h2>Plugin UI not built</h2>
<p>Run <code style="color:#3b82f6">cd examples/example-plugin/web && bun install && bun run build</code></p>
<p>Or set <code style="color:#3b82f6">FORCE_WEB_BUILD=1</code> before cargo build.</p>
</body>
</html>"#,
            );
        }
        return;
    }

    // Check that node_modules exists (bun install has been run)
    if !web_dir.join("node_modules").exists() {
        let status = Command::new("bun")
            .arg("install")
            .current_dir(&web_dir)
            .status()
            .expect("Failed to run `bun install`. Is bun installed?");
        if !status.success() {
            panic!("bun install failed");
        }
    }

    // Run vite build
    let status = Command::new("bun")
        .args(["run", "build"])
        .current_dir(&web_dir)
        .status()
        .expect("Failed to run `bun run build`. Is bun installed?");

    if !status.success() {
        panic!("Vite build failed");
    }

    assert!(
        dist_dir.join("index.html").exists(),
        "Vite build did not produce dist/index.html"
    );
}
