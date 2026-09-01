// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Build all workspace sandbox Docker images locally.
//!
//! This example calls the *real* `dockerfile_for_runtime` function used by
//! the production sandbox provider, writes each generated Dockerfile to a
//! tempdir, and shells out to `docker build` for every preset runtime. The
//! point is to validate the Dockerfiles end-to-end (especially after changes
//! to the Claude install path) without spinning up the full server.
//!
//! Usage:
//!     cargo run -p temps-agents --example build_sandbox_images
//!
//! Pass `--only <runtime>` to build a single image, or `--no-cache` to force
//! a clean rebuild.
//!
//! All output is streamed live to stdout/stderr so you can see progress.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Instant;

use temps_agents::sandbox::docker::{dockerfile_for_runtime, image_name_for_runtime};
use temps_agents::sandbox::pty_agent_bundle::BUNDLE as PTY_AGENT_BUNDLE;

const RUNTIMES: &[&str] = &["node", "bun", "python", "rust", "go", "full"];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let no_cache = args.iter().any(|a| a == "--no-cache");
    let only: Option<String> = args
        .iter()
        .position(|a| a == "--only")
        .and_then(|i| args.get(i + 1).cloned());

    let runtimes: Vec<&str> = match &only {
        Some(name) => {
            if !RUNTIMES.contains(&name.as_str()) {
                eprintln!(
                    "error: unknown runtime '{}'. valid: {}",
                    name,
                    RUNTIMES.join(", ")
                );
                std::process::exit(1);
            }
            vec![name.as_str()]
        }
        None => RUNTIMES.to_vec(),
    };

    println!("Building {} sandbox image(s):", runtimes.len());
    for r in &runtimes {
        println!("  - {} -> {}", r, image_name_for_runtime(r));
    }
    println!();

    let mut results: Vec<(String, Result<(), String>, std::time::Duration)> = Vec::new();
    let total_start = Instant::now();

    for runtime in &runtimes {
        let image = image_name_for_runtime(runtime);
        let dockerfile = dockerfile_for_runtime(runtime);

        println!("\n========================================================");
        println!("Building {} (runtime: {})", image, runtime);
        println!("========================================================");

        let start = Instant::now();
        let result = build_one(runtime, &image, &dockerfile, no_cache);
        let elapsed = start.elapsed();

        match &result {
            Ok(()) => println!("\n[ok] {} built in {:.1}s", image, elapsed.as_secs_f64()),
            Err(e) => println!(
                "\n[fail] {} after {:.1}s: {}",
                image,
                elapsed.as_secs_f64(),
                e
            ),
        }
        results.push((image, result, elapsed));
    }

    println!("\n========================================================");
    println!(
        "Summary ({} total, {:.1}s):",
        results.len(),
        total_start.elapsed().as_secs_f64()
    );
    println!("========================================================");
    let mut failed = 0;
    for (image, result, elapsed) in &results {
        match result {
            Ok(()) => println!("  ok    {:<32} {:>6.1}s", image, elapsed.as_secs_f64()),
            Err(e) => {
                println!(
                    "  FAIL  {:<32} {:>6.1}s   {}",
                    image,
                    elapsed.as_secs_f64(),
                    e
                );
                failed += 1;
            }
        }
    }

    if failed > 0 {
        eprintln!("\n{} image(s) failed to build", failed);
        std::process::exit(1);
    }
}

fn build_one(runtime: &str, image: &str, dockerfile: &str, no_cache: bool) -> Result<(), String> {
    // Write the dockerfile to a per-runtime tempdir. Using a unique dir per
    // runtime keeps parallel runs safe and lets us re-inspect the generated
    // dockerfile after the fact if a build fails.
    let tmpdir = std::env::temp_dir().join(format!("temps-sandbox-build-{}", runtime));
    std::fs::create_dir_all(&tmpdir)
        .map_err(|e| format!("create tempdir {}: {}", tmpdir.display(), e))?;
    let dockerfile_path = tmpdir.join("Dockerfile");
    {
        let mut f = std::fs::File::create(&dockerfile_path)
            .map_err(|e| format!("create dockerfile: {}", e))?;
        f.write_all(dockerfile.as_bytes())
            .map_err(|e| format!("write dockerfile: {}", e))?;
    }
    println!("(dockerfile written to {})", dockerfile_path.display());

    // Materialize the pty-agent bundle into the build context. Matches the
    // in-memory tar the server would ship to the Docker daemon — keeping
    // the local `docker build` path semantically identical to production
    // `build_image_locally`.
    for file in PTY_AGENT_BUNDLE {
        let dest = tmpdir.join(file.path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {}", parent.display(), e))?;
        }
        let mut out = std::fs::File::create(&dest)
            .map_err(|e| format!("create {}: {}", dest.display(), e))?;
        out.write_all(file.contents)
            .map_err(|e| format!("write {}: {}", dest.display(), e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&dest)
                .map_err(|e| format!("stat {}: {}", dest.display(), e))?
                .permissions();
            perms.set_mode(file.mode);
            std::fs::set_permissions(&dest, perms)
                .map_err(|e| format!("chmod {}: {}", dest.display(), e))?;
        }
    }

    let mut cmd = Command::new("docker");
    cmd.arg("build")
        .arg("-t")
        .arg(image)
        .arg("-f")
        .arg(&dockerfile_path)
        .arg(&tmpdir);
    if no_cache {
        cmd.arg("--no-cache");
    }
    cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());

    let status = cmd
        .status()
        .map_err(|e| format!("failed to spawn docker build: {}", e))?;
    if !status.success() {
        return Err(format!("docker build exited with {}", status));
    }
    Ok(())
}
