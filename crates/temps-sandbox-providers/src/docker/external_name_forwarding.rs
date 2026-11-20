// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::docker::{create_host_isolated_network, isolated_sandbox_network_request};
use bollard::container::LogOutput;
use bollard::models::{ContainerCreateBody, HostConfig};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, LogsOptions, RemoveContainerOptions, WaitContainerOptions,
};
use bollard::Docker;
use futures::StreamExt;

const IMAGE: &str = "alpine:3.20";

/// Runs `script` in a throwaway container and returns its exit code
/// and combined output.
async fn run(
    docker: &Docker,
    name: &str,
    network: Option<&str>,
    script: &str,
) -> Result<(i64, String), bollard::errors::Error> {
    docker
        .create_container(
            Some(CreateContainerOptionsBuilder::new().name(name).build()),
            ContainerCreateBody {
                image: Some(IMAGE.to_string()),
                cmd: Some(vec!["sh".into(), "-c".into(), script.into()]),
                host_config: Some(HostConfig {
                    network_mode: network.map(str::to_string),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await?;
    let result = async {
        docker.start_container(name, None).await?;
        let mut wait = docker.wait_container(name, None::<WaitContainerOptions>);
        let code = match wait.next().await {
            Some(Ok(status)) => status.status_code,
            // A non-zero exit is reported as an error by bollard.
            Some(Err(bollard::errors::Error::DockerContainerWaitError { code, .. })) => code,
            Some(Err(error)) => return Err(error),
            None => -1,
        };
        let mut logs = docker.logs(
            name,
            Some(LogsOptions {
                stdout: true,
                stderr: true,
                ..Default::default()
            }),
        );
        let mut output = String::new();
        while let Some(chunk) = logs.next().await {
            match chunk? {
                LogOutput::StdOut { message } | LogOutput::StdErr { message } => {
                    output.push_str(&String::from_utf8_lossy(&message))
                }
                _ => {}
            }
        }
        Ok((code, output))
    }
    .await;
    let _ = docker
        .remove_container(
            name,
            Some(RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await;
    result
}

#[tokio::test]
async fn sandbox_bridge_does_not_forward_external_names() {
    let Ok(docker) = Docker::connect_with_local_defaults() else {
        println!("Docker not available, skipping");
        return;
    };
    if docker.ping().await.is_err() {
        println!("Docker not responding, skipping");
        return;
    }
    if docker.inspect_image(IMAGE).await.is_err() {
        println!("{IMAGE} is not present locally, skipping");
        return;
    }
    let id = format!("{:012x}", rand::random::<u64>() & 0xffff_ffff_ffff);
    // The libc resolver, as an application uses it: busybox `nslookup`
    // tries only the host's search domain when it has one, so it fails
    // a peer's bare name and proves nothing about external ones.
    let lookup = "getent hosts example.com >/dev/null 2>&1; echo ext=$?";

    // Control: with a normal network the host resolves the name, so
    // a failure below is the network's doing, not an offline host.
    match run(&docker, &format!("dns-control-{id}"), None, lookup).await {
        Ok((_, output)) if output.contains("ext=0") => {}
        other => {
            println!("External DNS is unavailable on this host ({other:?}), skipping");
            return;
        }
    }

    let container = format!("temps-sandbox-dns-probe-{id}");
    let network = format!("temps-sandbox-net-dns-probe-{id}");
    let mut request = isolated_sandbox_network_request(
        &network,
        &container,
        crate::preview_gateway::PREVIEW_GATEWAY_CONTAINER,
    );
    // Pinned, so the test does not need a free default address pool.
    request.ipam = Some(bollard::models::Ipam {
        config: Some(vec![bollard::models::IpamConfig {
            subnet: Some(format!(
                "10.{}.{}.0/24",
                16 + rand::random::<u8>() % 200,
                rand::random::<u8>()
            )),
            ..Default::default()
        }]),
        ..Default::default()
    });
    if let Err(error) = create_host_isolated_network(&docker, request).await {
        println!("Could not create a test network ({error}), skipping");
        return;
    }

    // A peer on the same network, so the probe also shows that
    // internal names still resolve: the resolver is up, it just
    // does not forward.
    let peer = format!("dns-peer-{id}");
    let peer_started = async {
        docker
            .create_container(
                Some(CreateContainerOptionsBuilder::new().name(&peer).build()),
                ContainerCreateBody {
                    image: Some(IMAGE.to_string()),
                    cmd: Some(vec!["sleep".into(), "60".into()]),
                    host_config: Some(HostConfig {
                        network_mode: Some(network.clone()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await?;
        docker.start_container(&peer, None).await
    }
    .await;
    let probe = match &peer_started {
        Ok(()) => Some(
            run(
                &docker,
                &container,
                Some(&network),
                &format!("{lookup}; getent hosts {peer} >/dev/null 2>&1; echo int=$?"),
            )
            .await,
        ),
        Err(_) => None,
    };

    // Never leave the test containers or network behind.
    let _ = docker
        .remove_container(
            &peer,
            Some(RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await;
    let _ = docker.remove_network(&network).await;

    peer_started.expect("start the peer container");
    let (_, output) = probe
        .expect("the probe ran")
        .expect("run the probe container");
    assert!(
        output.contains("int=0"),
        "a peer's name must resolve on the sandbox network: {output}"
    );
    assert!(
        !output.contains("ext=0"),
        "Docker's embedded DNS forwarded an external name for a container \
             attached only to a sandbox's isolated bridge; sandboxes can leak \
             data through DNS: {output}"
    );
}
