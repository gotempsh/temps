// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use temps_agent_runtime::{
    lifecycle::RuntimeId,
    protocol_client::{
        RemoteRuntimeClient, RemoteRuntimeConnection, RemoteRuntimeConnector, RemoteTransportError,
    },
    protocol_stream::AuthenticatedStreamConnection,
    retained::{RuntimeClient, RuntimeSpec},
    Provider,
};
use tokio::process::Command;

struct CliConnector {
    container: String,
}

#[async_trait]
impl RemoteRuntimeConnector for CliConnector {
    async fn connect(&self) -> Result<Arc<dyn RemoteRuntimeConnection>, RemoteTransportError> {
        let mut child = Command::new("docker")
            .args([
                "exec",
                "-i",
                &self.container,
                "temps-sandbox-runtime",
                "connect",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| RemoteTransportError::Disconnected {
                message: "failed to start retained runtime CLI frontend".into(),
            })?;
        let reader = child
            .stdout
            .take()
            .ok_or_else(|| RemoteTransportError::Disconnected {
                message: "retained runtime CLI frontend has no stdout".into(),
            })?;
        let writer = child
            .stdin
            .take()
            .ok_or_else(|| RemoteTransportError::Disconnected {
                message: "retained runtime CLI frontend has no stdin".into(),
            })?;
        // The pipe handles keep the child alive. Its kill-on-drop guard ensures
        // reconnects do not leave frontend processes behind after a client drops.
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
        Ok(Arc::new(AuthenticatedStreamConnection::new(reader, writer)))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let container = std::env::args()
        .nth(1)
        .ok_or("provide the disposable Docker container name")?;
    let runtime_id = RuntimeId::new("docker-retained-smoke")?;
    let connector = Arc::new(CliConnector { container });
    let first = RemoteRuntimeClient::new(connector.clone());
    let acquired = first
        .acquire(RuntimeSpec::new(
            runtime_id.clone(),
            Provider::Codex,
            PathBuf::from("/home/temps/workspace"),
        ))
        .await?;
    if acquired.runtime_id() != &runtime_id {
        return Err("acquire returned a different runtime identity".into());
    }
    drop(acquired);
    drop(first);

    let second = RemoteRuntimeClient::new(connector);
    let attached = second.attach(&runtime_id).await?;
    if attached.runtime_id() != &runtime_id {
        return Err("attach returned a different runtime identity".into());
    }
    drop(attached);
    second.dispose(&runtime_id).await?;
    if second.attach(&runtime_id).await.is_ok() {
        return Err("disposed runtime unexpectedly remained attachable".into());
    }
    let missing = RuntimeId::new("docker-retained-missing")?;
    if second.attach(&missing).await.is_ok() {
        return Err("nonexistent runtime unexpectedly attached".into());
    }
    println!("retained-acquire-reattach-dispose-ok");
    Ok(())
}
