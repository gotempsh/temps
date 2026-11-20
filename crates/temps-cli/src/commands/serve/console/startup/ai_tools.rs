// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Populates the AI API tool handles once the admin router exists (ADR-024).

use super::*;

/// Fill the `ApiToolsHandle` (read tool) and `WriteApiToolsHandle` (write
/// tool) with callers that replay requests through `admin`, restricted to the
/// allowlists in `temps_ai_api_tools::allowlists`. Non-fatal: when the
/// OpenAPI document cannot be built the tools are simply unavailable.
pub(super) fn install_ai_api_tools(plugin_manager: &PluginManager, admin: &Router) {
    let service_context = plugin_manager.service_context();
    if let Some(handle) = service_context.get_service::<temps_ai_api_tools::ApiToolsHandle>() {
        match create_openapi(plugin_manager) {
            Ok(openapi) => {
                // The admin router carries the full plugin API surface plus
                // the auth middleware stack (permission_guard! reads
                // AuthContext from extensions, which the middleware injects).
                // We clone it here so InternalApiCaller can replay synthetic
                // requests through it without consuming the original.
                //
                // ALLOWLIST for the AI `call_api` tool — see
                // `ai_read_allowlist()` below for the full curated list and
                // the security rationale behind what is/isn't included.
                let allowlist: Vec<String> = ai_read_allowlist();
                // Plus the narrow, separately-vetted set of read-only POSTs
                // — see `ai_read_safe_posts()` for the rule governing it.
                let safe_posts: Vec<String> = ai_read_safe_posts();
                let caller = temps_ai_api_tools::InternalApiCaller::new_allowlisted_with_safe_posts(
                    admin.clone(),
                    &openapi,
                    allowlist.clone(),
                    safe_posts.clone(),
                );
                // Diagnostic: report which allowlist entries actually
                // resolved to a real operation in the OpenAPI doc, and
                // loudly flag any that did not (a typo or a wrong
                // method/operation_id silently drops the op otherwise) —
                // mirrors the same check already done for the write
                // allowlist below.
                let resolved = caller.indexed_operation_ids();
                let unresolved: Vec<&String> = allowlist
                    .iter()
                    .chain(safe_posts.iter())
                    .filter(|id| !resolved.contains(id))
                    .collect();
                info!(
                    resolved_count = resolved.len(),
                    allowlist_count = allowlist.len() + safe_posts.len(),
                    "AI read tool: indexed read operations"
                );
                if !unresolved.is_empty() {
                    tracing::warn!(
                        ?unresolved,
                        "AI read tool: allowlisted read operations did NOT resolve to \
                         an OpenAPI operation and are unavailable — check the operation_id"
                    );
                }
                let registry =
                    service_context.require_service::<temps_ai_api_tools::PluginToolsRegistry>();
                let audit = service_context.require_service::<dyn temps_core::AuditLogger>();
                registry.bind(
                    caller,
                    temps_ai_api_tools::PluginToolAccess::Read,
                    audit.clone(),
                    move |caller| handle.set(caller),
                );
                debug!("ADR-024: InternalApiCaller populated in ApiToolsHandle");

                // ── Approval-aware WRITE tool ──
                // Populate the separate WriteApiToolsHandle with a method-aware
                // caller over a CURATED allowlist of mutating operations.
                // `temps_write` applies the active harness approval mode and
                // replays authorized mutations through this same router
                // (permission_guard! + audit). This allowlist is conservative
                // by design: high-value, mostly-reversible
                // lifecycle + config operations. Adding an entry is a product +
                // security decision (what may the AI propose for a human to run).
                if let Some(write_handle) =
                    service_context.get_service::<temps_ai_api_tools::WriteApiToolsHandle>()
                {
                    let write_allowlist: Vec<String> = ai_write_allowlist();
                    let write_caller = temps_ai_api_tools::InternalApiCaller::new_write_allowlisted(
                        admin.clone(),
                        &openapi,
                        write_allowlist.clone(),
                    );
                    // Diagnostic: report which allowlist entries actually
                    // resolved to a real operation in the OpenAPI doc, and
                    // loudly flag any that did not (a typo or a wrong
                    // method/operation_id silently drops the op otherwise).
                    let resolved = write_caller.indexed_operation_ids();
                    let unresolved: Vec<&String> = write_allowlist
                        .iter()
                        .filter(|id| !resolved.contains(id))
                        .collect();
                    info!(
                        resolved_count = resolved.len(),
                        allowlist_count = write_allowlist.len(),
                        resolved = ?resolved,
                        "AI write tool: indexed write operations"
                    );
                    if !unresolved.is_empty() {
                        tracing::warn!(
                            ?unresolved,
                            "AI write tool: allowlisted write operations did NOT resolve to \
                             an OpenAPI operation and are unavailable — check the operation_id"
                        );
                    }
                    registry.bind(
                        write_caller,
                        temps_ai_api_tools::PluginToolAccess::Write,
                        audit,
                        move |caller| write_handle.set(caller),
                    );
                    debug!("AI write tool: WriteApiToolsHandle populated (curated allowlist)");
                } else {
                    debug!("AI write tool: WriteApiToolsHandle not registered; skipping");
                }
            }
            Err(e) => {
                // Non-fatal: the AI API tools simply won't be available this
                // run.  Log the error so operators can diagnose it without
                // taking down the whole server.
                tracing::warn!(
                    error = %e,
                    "ADR-024: failed to build OpenAPI doc for InternalApiCaller; \
                     AI API tools will not be available"
                );
            }
        }
    } else {
        // AiChatPlugin is not loaded (e.g. AI is disabled). This is expected
        // in reduced-feature deployments; no action needed.
        debug!("ADR-024: ApiToolsHandle not registered (AiChatPlugin absent); skipping InternalApiCaller setup");
    }
}
