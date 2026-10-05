// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Build one image through `DockerRuntime`, the same code path a deployment
//! uses, and report how long it took. Used to compare build-time changes.
//!
//! Usage:
//!     cargo run -p temps-deployer --example build_bench -- \
//!         <context-dir> <dockerfile> <image-tag> <cache-namespace> <log-file>
//!
//! `cache-namespace` is passed as `BUILDKIT_CACHE_MOUNT_NS`, exactly as the
//! workflow planner does, so separate runs can be given separate caches.
//! The build log, including the per-step timing summary, goes to `log-file`;
//! stdout gets one JSON line with the wall time.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use bollard::Docker;
use temps_deployer::docker::DockerRuntime;
use temps_deployer::{BuildRequest, BuildRequestWithCallback, ImageBuilder};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [context, dockerfile, tag, namespace, log] = args.as_slice() else {
        eprintln!(
            "usage: build_bench <context-dir> <dockerfile> <image-tag> <cache-namespace> <log-file>"
        );
        std::process::exit(2);
    };

    let docker = match Docker::connect_with_local_defaults() {
        Ok(docker) => Arc::new(docker),
        Err(e) => {
            eprintln!("cannot connect to Docker: {e}");
            std::process::exit(1);
        }
    };
    let runtime = DockerRuntime::new(docker, true, "temps-bench".to_string());

    let request = BuildRequest {
        cache_from: Vec::new(),
        image_name: tag.clone(),
        context_path: PathBuf::from(context),
        dockerfile_path: Some(PathBuf::from(dockerfile)),
        build_args: HashMap::from([("BUILDKIT_CACHE_MOUNT_NS".to_string(), namespace.clone())]),
        build_args_buildkit: HashMap::new(),
        platform: None,
        log_path: PathBuf::from(log),
    };

    let started = Instant::now();
    let result = runtime
        .build_image_with_callback(BuildRequestWithCallback {
            request,
            log_callback: None,
        })
        .await;
    let wall_ms = started.elapsed().as_millis();

    match result {
        Ok(build) => println!(
            "{{\"tag\":\"{}\",\"ok\":true,\"wall_ms\":{},\"size_bytes\":{}}}",
            tag, wall_ms, build.size_bytes
        ),
        Err(e) => {
            println!("{{\"tag\":\"{tag}\",\"ok\":false,\"wall_ms\":{wall_ms}}}");
            eprintln!("build failed: {e}");
            std::process::exit(1);
        }
    }
}
