// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One-shot CI runner. OIDC is consumed once; the resulting token is scoped
//! to one build and is never saved to disk or printed.
use clap::Args;
use futures::StreamExt;
use std::{path::PathBuf, sync::Arc, time::Duration};
use temps_deployer::{
    build_protocol::{safe_extract_context, MAX_BUILD_CONTEXT_BYTES},
    build_verification::ImageDigest,
    runner_protocol::{RunnerClaim, RunnerFailure, RunnerResult, RunnerWork},
    BuildRequest, BuildRequestWithCallback, ImageBuilder,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Args)]
pub struct RunnerCommand {
    #[arg(long)]
    pub control_plane: String,
    #[arg(long)]
    pub build_id: uuid::Uuid,
    #[arg(long, default_value = "temps-build")]
    pub audience: String,
    /// Optional OIDC token file for a non-default issuer. Read once at startup.
    #[arg(long)]
    pub identity_token_file: Option<PathBuf>,
}
impl RunnerCommand {
    pub fn execute(self) -> anyhow::Result<()> {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(self.run())
    }
    async fn run(self) -> anyhow::Result<()> {
        let url = reqwest::Url::parse(&self.control_plane)?;
        if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
            anyhow::bail!("Runner control plane requires HTTPS without URL credentials");
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let identity = if let Some(path) = self.identity_token_file {
            let file = tokio::fs::File::open(path).await?;
            let mut bytes = Vec::new();
            file.take(32 * 1024 + 1).read_to_end(&mut bytes).await?;
            if bytes.len() > 32 * 1024 {
                anyhow::bail!("OIDC token file exceeds 32 KiB");
            }
            String::from_utf8(bytes)?.trim().to_string()
        } else {
            // These are CI-provided one-shot credential inputs, not runtime settings.
            let endpoint = std::env::var("ACTIONS_ID_TOKEN_REQUEST_URL").map_err(|_| {
                anyhow::anyhow!("CI must grant id-token: write, or provide --identity-token-file")
            })?;
            let request_token = std::env::var("ACTIONS_ID_TOKEN_REQUEST_TOKEN")
                .map_err(|_| anyhow::anyhow!("CI OIDC request credential missing"))?;
            let endpoint = reqwest::Url::parse(&endpoint)?;
            if endpoint.scheme() != "https" {
                anyhow::bail!("CI OIDC endpoint requires HTTPS");
            }
            #[derive(serde::Deserialize)]
            struct Identity {
                value: String,
            }
            let response = client
                .get(endpoint)
                .query(&[("audience", &self.audience)])
                .bearer_auth(request_token)
                .send()
                .await?
                .error_for_status()?;
            let identity: Identity = bounded_json(response, 32 * 1024).await?;
            identity.value
        };
        let base = format!(
            "{}/api/internal/ci-builds/{}",
            self.control_plane.trim_end_matches('/'),
            self.build_id
        );
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1800);
        let work: RunnerWork = loop {
            let response = client
                .post(format!("{base}/claim"))
                .json(&RunnerClaim {
                    identity_token: identity.clone(),
                })
                .send()
                .await?;
            if response.status() == reqwest::StatusCode::TOO_EARLY
                && tokio::time::Instant::now() < deadline
            {
                tokio::time::sleep(Duration::from_secs(3)).await;
                continue;
            }
            if !response.status().is_success() {
                anyhow::bail!("Build claim rejected: HTTP {}", response.status());
            }
            break bounded_json(response, 512 * 1024).await?;
        };
        work.spec.validate().map_err(anyhow::Error::msg)?;
        let token = work.token.clone();
        let build = build_and_push(&client, &base, &work, self.build_id);
        let keep_lease = async {
            loop {
                tokio::time::sleep(Duration::from_secs(3)).await;
                let response = client
                    .get(format!("{base}/lease"))
                    .bearer_auth(&token)
                    .send()
                    .await?;
                if !response.status().is_success() {
                    anyhow::bail!("CI build lease revoked: HTTP {}", response.status());
                }
            }
            #[allow(unreachable_code)]
            Ok::<(), anyhow::Error>(())
        };
        let result = tokio::select! {
            result = tokio::time::timeout(Duration::from_secs(u64::from(work.timeout_seconds)), build) => result.map_err(|_| anyhow::anyhow!("CI build deadline expired")).and_then(|result| result),
            result = keep_lease => result.and_then(|_| Err(anyhow::anyhow!("CI build lease ended"))),
            _ = tokio::signal::ctrl_c() => Err(anyhow::anyhow!("CI build cancelled")),
        };
        match result {
            Ok(result) => {
                let response = client
                    .post(format!("{base}/result"))
                    .bearer_auth(&token)
                    .json(&result)
                    .send()
                    .await?;
                if !response.status().is_success() {
                    anyhow::bail!("CI build result rejected: HTTP {}", response.status());
                }
                tracing::info!(build_id = %self.build_id, "CI build result verified by control plane");
                Ok(())
            }
            Err(error) => {
                let _ = client
                    .post(format!("{base}/failure"))
                    .bearer_auth(&token)
                    .json(&RunnerFailure {
                        message: error.to_string().chars().take(1024).collect(),
                    })
                    .send()
                    .await;
                Err(error)
            }
        }
    }
}
async fn bounded_json<T: serde::de::DeserializeOwned>(
    mut response: reqwest::Response,
    max: usize,
) -> anyhow::Result<T> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len().saturating_add(chunk.len()) > max {
            anyhow::bail!("Runner API response exceeds size limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&bytes)?)
}
async fn build_and_push(
    client: &reqwest::Client,
    base: &str,
    work: &RunnerWork,
    id: uuid::Uuid,
) -> anyhow::Result<RunnerResult> {
    let scratch = tempfile::tempdir()?;
    let archive_path = scratch.path().join("context.tar");
    let mut file = tokio::fs::File::create(&archive_path).await?;
    let mut response = client
        .get(format!("{base}/context"))
        .bearer_auth(&work.token)
        .timeout(Duration::from_secs(1800))
        .send()
        .await?
        .error_for_status()?;
    let mut total = 0_u64;
    while let Some(chunk) = response.chunk().await? {
        total = total.saturating_add(chunk.len() as u64);
        if total > MAX_BUILD_CONTEXT_BYTES {
            anyhow::bail!("CI build context exceeds agent protocol limit");
        }
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    drop(file);
    let context = scratch.path().join("source");
    tokio::fs::create_dir(&context).await?;
    let extract_context = context.clone();
    tokio::task::spawn_blocking(move || safe_extract_context(&archive_path, &extract_context))
        .await?
        .map_err(anyhow::Error::msg)?;
    let docker = Arc::new(bollard::Docker::connect_with_defaults()?);
    docker.ping().await?;
    let builder =
        temps_deployer::docker::DockerRuntime::new(docker.clone(), true, "temps-ci-runner".into());
    let mut args: std::collections::HashMap<_, _> =
        work.spec.build_args.clone().into_iter().collect();
    if let Some(namespace) = &work.spec.cache_namespace {
        args.insert(
            temps_deployer::build_protocol::CACHE_MOUNT_NAMESPACE_ARG.into(),
            namespace.clone(),
        );
    }
    let (log_tx, mut log_rx) = tokio::sync::mpsc::channel::<String>(64);
    let log_callback: temps_deployer::LogCallback = Arc::new(move |line| {
        let tx = log_tx.clone();
        Box::pin(async move {
            let _ = tx.send(line.chars().take(2048).collect()).await;
        })
    });
    let upload_logs = async {
        while let Some(line) = log_rx.recv().await {
            let mut lines = vec![line];
            let mut size = lines[0].len();
            let flush_at = tokio::time::Instant::now() + Duration::from_millis(100);
            while lines.len() < 8 && size < 48 * 1024 {
                match tokio::time::timeout_at(flush_at, log_rx.recv()).await {
                    Ok(Some(line)) => {
                        size += line.len();
                        lines.push(line);
                    }
                    Ok(None) | Err(_) => break,
                }
            }
            let response = client
                .post(format!("{base}/logs"))
                .bearer_auth(&work.token)
                .json(&temps_deployer::runner_protocol::RunnerLogs { lines })
                .send()
                .await?;
            if !response.status().is_success() {
                anyhow::bail!("CI build log upload rejected: HTTP {}", response.status());
            }
        }
        Ok::<(), anyhow::Error>(())
    };
    let build = builder.build_image_with_callback(BuildRequestWithCallback {
        request: BuildRequest {
            context_path: context.clone(),
            dockerfile_path: Some(context.join(&work.spec.dockerfile)),
            image_name: work.registry_tag.clone(),
            build_args: args,
            build_args_buildkit: Default::default(),
            cache_from: Vec::new(),
            platform: work.spec.platform.clone(),
            log_path: scratch.path().join("build.log"),
        },
        log_callback: Some(log_callback),
    });
    let (built, ()) = tokio::try_join!(
        async { build.await.map_err(anyhow::Error::from) },
        upload_logs
    )?;
    let image = builder.inspect_image(&built.image_name).await?;
    if !work
        .spec
        .platform
        .as_ref()
        .is_some_and(|p| temps_deployer::platform::platforms_match(p, &image.platform))
    {
        anyhow::bail!("CI runner produced the wrong architecture");
    }
    let (repository, tag) = work
        .registry_tag
        .rsplit_once(':')
        .ok_or_else(|| anyhow::anyhow!("Invalid server registry tag"))?;
    let credentials = work
        .registry_credentials
        .clone()
        .map(|c| bollard::auth::DockerCredentials {
            username: c.username,
            password: c.password,
            identitytoken: c.identity_token,
            serveraddress: c.server_address,
            ..Default::default()
        });
    let mut pushes = docker.push_image(
        repository,
        Some(bollard::query_parameters::PushImageOptions {
            tag: Some(tag.into()),
            platform: None,
        }),
        credentials,
    );
    let mut digest = None;
    while let Some(event) = pushes.next().await {
        let event = event?;
        if event.error_detail.is_some() {
            anyhow::bail!("CI registry push failed");
        }
        if let Some(status) = event.status {
            for word in status.split_whitespace() {
                if let Ok(value) = ImageDigest::parse(word) {
                    digest = Some(value.to_string());
                }
            }
        }
    }
    let digest = match digest {
        Some(digest) => digest,
        None => docker
            .inspect_image(&work.registry_tag)
            .await?
            .repo_digests
            .unwrap_or_default()
            .into_iter()
            .find_map(|entry| {
                entry
                    .strip_prefix(&format!("{repository}@"))
                    .and_then(|digest| ImageDigest::parse(digest).ok())
                    .map(|digest| digest.to_string())
            })
            .ok_or_else(|| anyhow::anyhow!("Registry push did not report a manifest digest"))?,
    };
    Ok(RunnerResult {
        build_id: id.to_string(),
        run_id: work.run_id.clone(),
        digest,
        platforms: vec![image.platform.clone()],
        image,
        duration_ms: built.build_duration_ms,
    })
}
