// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! Opt-in real-provider test: build the three :*-dev-4 images before running.
use std::{collections::HashMap, sync::Arc, time::Duration};
use temps_agents::sandbox::{
    docker::{DockerSandboxConfig, DockerSandboxProvider},
    SandboxCreateConfig, SandboxProvider,
};

#[tokio::test]
async fn creates_flavors_and_executes_through_runtime_daemon() {
    let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
        return;
    };
    if docker.ping().await.is_err() {
        eprintln!("SKIP: Docker unavailable");
        return;
    }
    for flavor in ["nodejs", "python", "all"] {
        let image = format!("temps-sandbox-runtime:{flavor}-dev-4");
        if docker.inspect_image(&image).await.is_err() {
            eprintln!("SKIP: build {image} using tools/sandbox-runtime/build-local.sh first");
            return;
        }
    }
    let provider = DockerSandboxProvider::new(Arc::new(docker), DockerSandboxConfig::default());
    for flavor in ["nodejs", "python", "all"] {
        let workspace = tempfile::tempdir().unwrap();
        let label = format!(
            "runtime-test-{}-{}-{flavor}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        );
        let handle = provider
            .create(SandboxCreateConfig {
                run_id: 0,
                container_name_override: Some(label),
                host_work_dir: workspace.path().into(),
                workspace_volume: None,
                image: Some(format!("temps-sandbox-runtime:{flavor}-dev-4")),
                cpu_limit: Some(1.0),
                memory_limit_mb: Some(512),
                pids_limit: Some(128),
                disk_size_mb: None,
                network_mode: Some("none".into()),
                env_vars: HashMap::new(),
                idle_timeout: Duration::from_secs(60),
                backend: None,
                owner_user_id: None,
            })
            .await
            .unwrap();
        // /proc parent proves the command was spawned by the resident daemon,
        // not merely docker exec with a daemon sitting idle beside it.
        let output = provider.exec(&handle, vec!["node".into(), "-e".into(),
            "const fs=require('fs');console.log(fs.readFileSync('/proc/'+process.ppid+'/cmdline','utf8'));console.error('stderr-ok');process.exit(7)".into()], HashMap::new(), None).await;
        let flavor_output = provider
            .exec(
                &handle,
                match flavor {
                    "python" => vec!["python3".into(), "--version".into()],
                    "all" => vec!["go".into(), "version".into()],
                    _ => vec!["node".into(), "--version".into()],
                },
                HashMap::new(),
                None,
            )
            .await;
        let large_output = provider.exec(&handle, vec!["node".into(), "-e".into(),
            "process.stdout.write(JSON.stringify({text:'é'.repeat(20000),env:process.env.RUNTIME_TEST_VALUE}))".into()],
            HashMap::from([("RUNTIME_TEST_VALUE".into(), "forwarded".into())]), None).await;
        let missing = provider
            .exec(
                &handle,
                vec!["/missing-runtime-command".into()],
                HashMap::new(),
                None,
            )
            .await;
        let start = provider.exec(&handle, vec!["temps-sandbox-runtime".into(), "request".into(),
            "/run/temps-runtime/control.sock".into(),
            r#"{"version":1,"operation":{"type":"start","name":"persistent-test","program":"node","args":["-e","setInterval(()=>{},1000)"],"directory":"."}}"#.into()], HashMap::new(), None).await;
        // Separate provider calls represent separate turns/clients.
        let list = provider
            .exec(
                &handle,
                vec![
                    "temps-sandbox-runtime".into(),
                    "request".into(),
                    "/run/temps-runtime/control.sock".into(),
                    r#"{"version":1,"operation":{"type":"list"}}"#.into(),
                ],
                HashMap::new(),
                None,
            )
            .await;
        let cleanup = provider.destroy(&handle, true).await;
        let output = output.unwrap();
        assert!(
            output.stdout.contains("temps-sandbox-runtime"),
            "{}",
            output.stdout
        );
        assert!(output.stderr.contains("stderr-ok"));
        assert_eq!(output.exit_code, 7);
        assert_eq!(flavor_output.unwrap().exit_code, 0);
        let large_output = large_output.unwrap();
        assert_eq!(large_output.exit_code, 0);
        let json: serde_json::Value = serde_json::from_str(&large_output.stdout).unwrap();
        assert_eq!(json["text"], "é".repeat(20000));
        assert_eq!(json["env"], "forwarded");
        let missing = missing.unwrap();
        assert_ne!(missing.exit_code, 0);
        assert!(
            missing.stderr.contains("spawn runtime command"),
            "{}",
            missing.stderr
        );
        assert_eq!(start.unwrap().exit_code, 0);
        let list: serde_json::Value = serde_json::from_str(&list.unwrap().stdout).unwrap();
        assert_eq!(list["processes"][0]["status"], "running");
        cleanup.unwrap();
    }
}
