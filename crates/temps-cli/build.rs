use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let dist_dir = manifest_dir.join("dist");

    // Allow skipping web build during development
    if env::var("SKIP_WEB_BUILD").is_ok() {
        println!("cargo:warning=Skipping web build (SKIP_WEB_BUILD is set)");
        ensure_placeholder_dist(&dist_dir);
        return;
    }

    // Only build web in release mode by default (unless FORCE_WEB_BUILD is set)
    let profile = env::var("PROFILE").unwrap_or_default();
    if profile == "debug" && env::var("FORCE_WEB_BUILD").is_err() {
        println!("cargo:warning=Skipping web build in debug mode (use FORCE_WEB_BUILD=1 to build)");
        ensure_placeholder_dist(&dist_dir);
        return;
    }

    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("Failed to get workspace root");

    let web_dir = workspace_root.join("web");

    if !web_dir.exists() {
        println!(
            "cargo:warning=Web directory not found at {}, skipping web build",
            web_dir.display()
        );
        return;
    }

    // Tell Cargo when to rebuild
    println!("cargo:rerun-if-changed={}/package.json", web_dir.display());
    println!("cargo:rerun-if-changed={}/src", web_dir.display());
    println!("cargo:rerun-if-changed={}/public", web_dir.display());
    println!(
        "cargo:rerun-if-changed={}/rsbuild.config.ts",
        web_dir.display()
    );
    println!("cargo:rerun-if-env-changed=SKIP_WEB_BUILD");
    println!("cargo:rerun-if-env-changed=FORCE_WEB_BUILD");

    build_web(&web_dir, &dist_dir);
}

fn build_web(web_dir: &std::path::Path, dist_dir: &std::path::Path) {
    println!("cargo:warning=Building web UI at {}...", web_dir.display());

    if !check_command("bun") {
        eprintln!("ERROR: bun not found. Install from https://bun.sh");
        std::process::exit(1);
    }

    // Install dependencies
    println!("cargo:warning=Running bun install...");
    run_cmd("bun", &["install"], web_dir);

    // Build with custom output directory
    println!("cargo:warning=Running bun run build...");
    println!("cargo:warning=Output directory: {}", dist_dir.display());

    // Set RSBUILD_OUTPUT_PATH environment variable for rsbuild
    let status = Command::new("bun")
        .args(["run", "build"])
        .current_dir(web_dir)
        .env("RSBUILD_OUTPUT_PATH", dist_dir)
        .status()
        .unwrap_or_else(|e| {
            eprintln!("Failed to execute bun run build: {}", e);
            std::process::exit(1);
        });

    if !status.success() {
        eprintln!("bun run build failed with status: {}", status);
        std::process::exit(1);
    }

    // Verify the build output exists
    if !dist_dir.exists() {
        eprintln!(
            "ERROR: Build completed but dist directory not found at {}",
            dist_dir.display()
        );
        std::process::exit(1);
    }

    // Check for index.html
    let index_html = dist_dir.join("index.html");
    if !index_html.exists() {
        eprintln!(
            "ERROR: Build completed but index.html not found at {}",
            index_html.display()
        );
        std::process::exit(1);
    }

    println!("cargo:warning=Web build complete!");
    println!(
        "cargo:warning=Static files available at: {}",
        dist_dir.display()
    );
}

fn check_command(cmd: &str) -> bool {
    Command::new(cmd).arg("--version").output().is_ok()
}

fn run_cmd(cmd: &str, args: &[&str], dir: &std::path::Path) {
    let status = Command::new(cmd)
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|e| {
            eprintln!("Failed to execute {} {:?}: {}", cmd, args, e);
            std::process::exit(1);
        });

    if !status.success() {
        eprintln!("{} {:?} failed with status: {}", cmd, args, status);
        std::process::exit(1);
    }
}

/// Ensure a placeholder dist directory exists for include_dir! macro
/// This prevents build errors when web build is skipped in debug mode
fn ensure_placeholder_dist(dist_dir: &std::path::Path) {
    // Create dist directory if it doesn't exist
    if !dist_dir.exists() {
        fs::create_dir_all(dist_dir).unwrap_or_else(|e| {
            eprintln!("Failed to create dist directory: {}", e);
            std::process::exit(1);
        });
        println!(
            "cargo:warning=Created placeholder dist directory at {}",
            dist_dir.display()
        );
    }

    // Create a placeholder index.html if it doesn't exist
    let index_html = dist_dir.join("index.html");
    if !index_html.exists() {
        let placeholder_html = r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Temps - Development Build</title>
    <style>
        body {
            font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
            display: flex;
            align-items: center;
            justify-content: center;
            height: 100vh;
            margin: 0;
            background: linear-gradient(135deg, #667eea 0%, #764ba2 100%);
            color: white;
        }
        .container {
            text-align: center;
            padding: 2rem;
            background: rgba(255, 255, 255, 0.1);
            border-radius: 1rem;
            backdrop-filter: blur(10px);
        }
        h1 { margin: 0 0 1rem 0; }
        p { margin: 0.5rem 0; opacity: 0.9; }
        code {
            background: rgba(0, 0, 0, 0.3);
            padding: 0.25rem 0.5rem;
            border-radius: 0.25rem;
            font-family: monospace;
        }
    </style>
</head>
<body>
    <div class="container">
        <h1>⚡ Temps Development Build</h1>
        <p>Web UI not built (debug mode)</p>
        <p>To build the web UI, run:</p>
        <p><code>FORCE_WEB_BUILD=1 cargo build</code></p>
        <p style="margin-top: 1.5rem; opacity: 0.7; font-size: 0.875rem;">
            Or build in release mode: <code>cargo build --release</code>
        </p>
    </div>
</body>
</html>"#;

        fs::write(&index_html, placeholder_html).unwrap_or_else(|e| {
            eprintln!("Failed to create placeholder index.html: {}", e);
            std::process::exit(1);
        });

        println!("cargo:warning=Created placeholder index.html for development");
    }
}
