// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What can answer a request without a project: challenges, then redirects.

use super::super::*;
use super::Flow;

impl LoadBalancer {
    /// The challenge solver's files, for a host without a project: a project's host was answered with Shield.
    pub(super) async fn solver_files_stage(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<Flow> {
        // The challenge solver, for hosts without a project (project hosts
        // were answered with Shield above).
        if self.serve_solver_asset(session, ctx).await? {
            return Ok(Flow::Answered);
        }

        Ok(Flow::Continue)
    }
}
