// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use crate::dev::{DevPollResponse, DevReply, DevSessionResponse, DevTunnel};
impl WorkflowService {
    pub fn with_dev(mut self, dev: Arc<DevTunnel>) -> Self {
        self.dev = Some(dev);
        self
    }
    fn require_dev(&self, environment_id: i32) -> WorkflowResult<&DevTunnel> {
        self.dev.as_deref().ok_or(WorkflowError::DevTunnel {
            environment_id,
            reason: "development tunnel service is not installed",
        })
    }
    pub async fn open_dev(
        &self,
        project: i32,
        environment: i32,
    ) -> WorkflowResult<DevSessionResponse> {
        let result = self
            .require_dev(environment)?
            .open(project, environment)
            .await?;
        self.engine.invalidate_manifest(environment);
        Ok(result)
    }
    pub async fn configure_dev(
        &self,
        project: i32,
        environment: i32,
        session: Uuid,
        manifest: crate::protocol::Manifest,
    ) -> WorkflowResult<()> {
        self.require_dev(environment)?
            .configure(project, environment, session, manifest)
            .await?;
        self.engine.invalidate_manifest(environment);
        Ok(())
    }
    pub async fn close_dev(
        &self,
        project: i32,
        environment: i32,
        session: Uuid,
    ) -> WorkflowResult<()> {
        self.require_dev(environment)?
            .close(project, environment, session)
            .await?;
        self.engine.invalidate_manifest(environment);
        Ok(())
    }
    pub async fn poll_dev(
        &self,
        project: i32,
        environment: i32,
        session: Uuid,
    ) -> WorkflowResult<DevPollResponse> {
        self.require_dev(environment)?
            .poll(project, environment, session)
            .await
    }
    pub async fn respond_dev(
        &self,
        project: i32,
        environment: i32,
        session: Uuid,
        request: Uuid,
        reply: DevReply,
    ) -> WorkflowResult<()> {
        self.require_dev(environment)?
            .respond(project, environment, session, request, reply)
            .await
    }
}
