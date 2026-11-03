// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Path routes (ADR 050): another service of the project serving a path.

use super::super::*;

impl LoadBalancer {
    /// Which service of the project serves this path, if not the host's own.
    pub(super) fn match_path_route(&self, ctx: &mut ProxyContext) {
        // Path routes (ADR 050): `frontend: /api -> backend` serves this
        // path from another service of the project. In-memory lookup only.
        //
        // The password form rendered for a path-routed target posts to the
        // reserved `/_temps/password-verify`, which never matches a path
        // route; `resolve_request_path_route` matches it by the routed path
        // the form carries, so the password is checked (and the cookie
        // issued) for the target environment the form was rendered for,
        // never the host's own. The Temps-login exchange callback is matched
        // the same way, by the `next` path its sign-in started from, so the
        // grant is traded (and the cookie scoped) for the routed target.
        // Client input only selects among the prefixes this host already
        // serves.
        if ctx.preview_route.is_none() {
            ctx.path_route = crate::handler::password_wall::resolve_request_path_route(
                &ctx.method,
                &ctx.path,
                ctx.query_string.as_deref(),
                |path| {
                    self.project_context_resolver
                        .match_path_route(&ctx.host, path)
                },
            );
        }
    }
}
