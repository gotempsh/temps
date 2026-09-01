// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * @temps-sdk/plugin -- Node.js SDK for building Temps external plugins.
 *
 * @example
 * ```ts
 * import { runPlugin, createManifest, extractAuthContext } from "@temps-sdk/plugin";
 *
 * runPlugin({
 *   manifest: () =>
 *     createManifest("hello-world", "0.1.0")
 *       .displayName("Hello World")
 *       .addNav("Hello", "hand", "/hello")
 *       .build(),
 *
 *   handler: (ctx) => async (req, res) => {
 *     const auth = extractAuthContext(req);
 *     const projects = await ctx.temps.listProjects();
 *
 *     res.writeHead(200, { "Content-Type": "application/json" });
 *     res.end(JSON.stringify({
 *       message: `Hello ${auth?.userEmail ?? "anonymous"}!`,
 *       projectCount: projects.length,
 *     }));
 *   },
 * });
 * ```
 */

// Runtime
export { runPlugin } from "./runtime.js";

// Manifest builder
export { ManifestBuilder, createManifest } from "./manifest-builder.js";

// Context
export { PluginContext } from "./context.js";

// Platform client
export { TempsClient } from "./client.js";

// Protocol helpers
export {
  extractAuthContext,
  HEADER_PLUGIN_NAME,
  HEADER_USER_ID,
  HEADER_USER_EMAIL,
  HEADER_USER_ROLE,
  HEADER_REQUEST_ID,
  HEADER_AUTH_SIGNATURE,
  PLUGIN_CHANNEL_PATH,
} from "./protocol.js";

// UI serving
export { createUiHandler, createEmbeddedUiHandler } from "./ui.js";
export type { EmbeddedFile, EmbeddedAssets } from "./ui.js";

// Errors
export {
  PluginSdkError,
  ArgsError,
  SocketBindError,
  HandshakeError,
  InitializationError,
  ChannelClosedError,
  PlatformError,
  ChannelTimeoutError,
} from "./errors.js";

// Types
export type {
  // Plugin definition
  TempsPlugin,
  RequestHandler,
  // Manifest
  PluginManifest,
  NavEntry,
  NavSection,
  UiManifest,
  UiRoute,
  // Protocol
  HandshakeMessage,
  ManifestMessage,
  ReadyMessage,
  ChannelMessage,
  ChannelRequest,
  ChannelResponse,
  ChannelEvent,
  ChannelError,
  ChannelErrorCode,
  // Events
  PluginEvent,
  // Data DTOs
  ProjectInfo,
  EnvironmentInfo,
  DeploymentInfo,
  // Auth
  TempsUserContext,
  // CLI
  PluginArgs,
} from "./types.js";
