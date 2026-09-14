// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::io;
use std::sync::Arc;

use async_trait::async_trait;
use futures::TryStreamExt;
use temps_agent_runtime::protocol_client::{
    RemoteRuntimeConnection, RemoteRuntimeConnector, RemoteTransportError,
};
use temps_agent_runtime::protocol_stream::AuthenticatedStreamConnection;
use temps_agents::sandbox::{SandboxHandle, SandboxProvider};
use tokio_util::io::StreamReader;

/// Opens a fresh authenticated daemon carrier inside one already-authorized sandbox.
pub(crate) struct SandboxRuntimeConnector {
    provider: Arc<dyn SandboxProvider>,
    handle: SandboxHandle,
}

impl SandboxRuntimeConnector {
    pub(crate) fn new(provider: Arc<dyn SandboxProvider>, handle: SandboxHandle) -> Self {
        Self { provider, handle }
    }
}

#[async_trait]
impl RemoteRuntimeConnector for SandboxRuntimeConnector {
    async fn connect(&self) -> Result<Arc<dyn RemoteRuntimeConnection>, RemoteTransportError> {
        let attachment = self
            .provider
            .connect_agent_runtime(&self.handle)
            .await
            .map_err(|_| RemoteTransportError::Rejected {
                message: format!(
                    "sandbox '{}' does not expose the retained runtime protocol",
                    self.handle.sandbox_name
                ),
            })?;
        let output = attachment.output.map_err(|_| {
            io::Error::new(
                io::ErrorKind::ConnectionReset,
                "sandbox runtime stream failed",
            )
        });
        let reader = StreamReader::new(output);
        Ok(Arc::new(AuthenticatedStreamConnection::new(
            reader,
            attachment.input,
        )))
    }
}
