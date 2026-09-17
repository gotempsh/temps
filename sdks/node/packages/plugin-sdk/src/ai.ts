// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { TempsClient } from "./client.js";
import type {
  PluginAiRequest,
  PluginAiResponse,
  PluginHostCapabilities,
} from "./types.js";

/**
 * Host-mediated AI. No provider key, user token, or claimed actor identity is needed.
 * Declare `.requestPermissions("ai_generate")` in the manifest and ask an
 * administrator to approve it in Settings → Plugins → Permissions.
 *
 * @example
 * ```ts
 * const access = await ctx.permissions();
 * if (access.ai.configured && access.permissions.includes("ai_generate")) {
 *   const result = await ctx.ai.generate({
 *     purpose: "page-audit",
 *     prompt: "Suggest a clearer title for this public page: ...",
 *     max_tokens: 512,
 *   });
 *   // Treat result.text as untrusted output; review before applying changes.
 * }
 * ```
 */
export class PluginAiClient {
  constructor(private readonly client: TempsClient) {}

  /** Includes live grants and setup information, including when AI is not configured. */
  capabilities(): Promise<PluginHostCapabilities> {
    return this.client.getHostCapabilities();
  }

  /**
   * The host enforces permission and usage limits, including for background jobs.
   * A grant may be revoked after capabilities() returns; handle permission_denied.
   */
  generate(input: PluginAiRequest): Promise<PluginAiResponse> {
    return this.client.generateAi(input);
  }
}
