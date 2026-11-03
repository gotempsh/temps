// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The order of `request_filter`.
//!
//! The order is what keeps these checks meaningful: each one protects what
//! comes after it, so moving one can open a bypass without any single check
//! changing. `order_tests` pins it: every test sets up a request that two
//! checks would both answer and asserts which one does.
//!
//! 1. Request facts, then inbound stripping of what only the proxy may set
//!    (`X-Temps-Demo-Mode`, identity headers, the status-host tag).
//! 2. Status-page hostname: answered here and nowhere else. A connection
//!    negotiated for such a hostname cannot carry any other host (421).
//! 3. Sandbox preview hostname. The public ingress hostname is never one.
//! 4. Signed-in gate (ADR 065 D8), before anything can wake or reach the app.
//! 5. Wake a sleeping host, after reserving a wake slot.
//! 6. Path route (ADR 050): match, wake a sleeping target after reserving a
//!    slot, and answer 502 when it has no target, so the host's own service
//!    never answers in its place.
//! 7. Project context: the path route's target if there is one, otherwise the
//!    host's. With a project, in order:
//!    1. connection cap;
//!    2. request policy gate (`Unavailable` 503, `Deny` 403);
//!    3. edge rules (ADR-089), except a password wall's own verify POST;
//!    4. the access decision (ADR-055 §11): IP restriction, blacklist, rate
//!       limit, ingress endpoint, Temps login. A login never blocks Shield's
//!       own paths;
//!    5. Shield's verify POST and solver files, a challenge stored for this
//!       host, then Shield itself (ADR 066);
//!    6. password wall, whose verify POST is rate limited before the password
//!       is checked.
//! 8. The solver files for a host without a project.
//! 9. ACME HTTP-01, before anything that could redirect the CA away.
//! 10. On-demand TLS's 503 on plain HTTP (ADR-018), before the HTTPS redirect.
//! 11. HTTPS redirect, then the domain's own redirect.
//! 12. Upstream request: strip client-supplied address headers, capture the
//!     headers for the request log, set the trusted forwarded headers.
//! 13. ACME and the domain redirect again, static files, persisted assets,
//!     stale chunks.
//! 14. Admin gate (ADR-051), last: it only decides requests about to fall
//!     through to the console.

/// Runs one stage: anything but `Continue` ends the stages around it.
macro_rules! stage {
    ($stage:expr) => {
        match $stage.await? {
            Flow::Continue => {}
            finished => return Ok(finished),
        }
    };
}

mod access;
mod admin_gate;
mod hostname;
mod inbound;
#[cfg(test)]
mod order_tests;
mod password_wall;
mod path_route;
mod project;
mod redirects;
mod static_content;
mod upstream;
mod wake;

use super::*;

/// What a stage of `request_filter` decided.
#[must_use]
pub(super) enum Flow {
    /// The next stage decides.
    Continue,
    /// The proxy answered the request itself: no later stage runs and nothing
    /// goes upstream.
    Answered,
    /// Send the request upstream as it is now, without running later stages.
    Forward,
}

impl Flow {
    /// A request is finished either way: `answered` says whether by the proxy
    /// or by sending it upstream.
    fn finished(answered: bool) -> Self {
        if answered {
            Self::Answered
        } else {
            Self::Forward
        }
    }
}

impl LoadBalancer {
    /// `request_filter`, in the order the module documentation states.
    pub(super) async fn run_request_stages(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<Flow> {
        self.read_request_facts(session, ctx)?;
        Self::strip_inbound_trust_headers(session);

        stage!(self.status_page_host_stage(session, ctx));

        stage!(self.preview_stage(session, ctx));

        stage!(self.signed_in_gate_stage(session, ctx));

        stage!(self.wake_sleeping_host_stage(session, ctx));

        self.match_path_route(ctx);

        stage!(self.wake_path_target_stage(session, ctx));

        stage!(self.unavailable_path_target_stage(session, ctx));

        let project_context = self.resolve_project_context(ctx).await;
        match &project_context {
            Some(project_ctx) => stage!(self.project_stage(session, ctx, project_ctx)),
            None => ctx.routing_status = "no_project".to_string(),
        }

        stage!(self.solver_files_stage(session, ctx));

        stage!(self.acme_challenge_stage(session, ctx, true));

        stage!(self.on_demand_tls_stage(session, ctx));

        stage!(self.https_redirect_stage(session, ctx));

        stage!(self.domain_redirect_stage(session, ctx));

        self.prepare_upstream_request(session, ctx)?;

        stage!(self.acme_challenge_stage(session, ctx, false));

        stage!(self.late_domain_redirect_stage(session, ctx));

        stage!(self.static_files_stage(session, ctx));

        stage!(self.persisted_asset_stage(session, ctx));

        stage!(self.stale_chunk_stage(session, ctx));

        self.admin_gate_stage(session, ctx).await
    }
}
