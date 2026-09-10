// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Types for the Temps external plugin protocol.
 *
 * These types mirror the Rust definitions in:
 * - crates/temps-core/src/external_plugin/manifest.rs
 * - crates/temps-core/src/external_plugin/channel.rs
 * - crates/temps-plugin-sdk/src/protocol.rs
 */

// ---------------------------------------------------------------------------
// Plugin Manifest
// ---------------------------------------------------------------------------

export type NavSection = "platform" | "settings" | "project";

export interface NavEntry {
  label: string;
  icon: string;
  section: NavSection;
  path: string;
  order: number;
}

export interface UiRoute {
  path: string;
  title: string;
}

export interface UiManifest {
  entry_js: string;
  css: string[];
  routes: UiRoute[];
}

export interface PluginManifest {
  name: string;
  version: string;
  display_name?: string;
  description?: string;
  nav: NavEntry[];
  ui?: UiManifest;
  requires_db: boolean;
  health_path: string;
  events: string[];
}

// ---------------------------------------------------------------------------
// Handshake Messages (stdout JSON lines)
// ---------------------------------------------------------------------------

export interface ManifestMessage {
  type: "manifest";
  name: string;
  version: string;
  display_name?: string;
  description?: string;
  nav: NavEntry[];
  ui?: UiManifest;
  requires_db: boolean;
  health_path: string;
  events: string[];
}

export interface ReadyMessage {
  type: "ready";
  ready: boolean;
  has_ui: boolean;
}

export type HandshakeMessage = ManifestMessage | ReadyMessage;

// ---------------------------------------------------------------------------
// Channel Protocol (WebSocket JSON frames)
// ---------------------------------------------------------------------------

export type ChannelErrorCode =
  | "method_not_found"
  | "invalid_params"
  | "permission_denied"
  | "not_found"
  | "internal";

export interface ChannelError {
  code: ChannelErrorCode;
  message: string;
}

export interface ChannelRequest {
  type: "request";
  id: number;
  method: string;
  params: Record<string, unknown>;
}

export interface ChannelResponse {
  type: "response";
  id: number;
  result?: unknown;
  error?: ChannelError;
}

export interface ChannelEvent {
  type: "event";
  event: PluginEvent;
}

export type ChannelMessage = ChannelRequest | ChannelResponse | ChannelEvent;

// ---------------------------------------------------------------------------
// Plugin Events
// ---------------------------------------------------------------------------

export interface PluginEvent {
  id: string;
  event_type: string;
  timestamp: string;
  project_id?: number;
  data: Record<string, unknown>;
}

// ---------------------------------------------------------------------------
// Platform Data DTOs (returned by TempsClient)
// ---------------------------------------------------------------------------

export interface ProjectInfo {
  id: number;
  name: string;
  slug: string;
  repo_name: string;
  repo_owner: string;
  main_branch: string;
  preset: string;
  source_type: string;
  created_at: string;
  updated_at: string;
  last_deployment?: string;
  enable_preview_environments: boolean;
}

export interface EnvironmentInfo {
  id: number;
  project_id: number;
  name: string;
  slug: string;
  branch?: string;
  is_preview: boolean;
  current_deployment_id?: number;
  created_at: string;
  updated_at: string;
}

export interface DeploymentInfo {
  id: number;
  project_id: number;
  environment_id: number;
  state: string;
  branch?: string;
  tag?: string;
  commit_sha?: string;
  commit_message?: string;
  commit_author?: string;
  created_at: string;
  started_at?: string;
  finished_at?: string;
}

// ---------------------------------------------------------------------------
// Auth Context (extracted from proxy headers)
// ---------------------------------------------------------------------------

export interface TempsUserContext {
  userId?: number;
  userEmail?: string;
  role: string;
  requestId: string;
}

// ---------------------------------------------------------------------------
// Plugin CLI Arguments
// ---------------------------------------------------------------------------

export interface PluginArgs {
  socketPath: string;
  databaseUrl?: string;
  authSecret: string;
  dataDir: string;
}

// ---------------------------------------------------------------------------
// Plugin Definition (user-facing interface)
// ---------------------------------------------------------------------------

import type { PluginContext } from "./context.js";
import type { EmbeddedAssets } from "./ui.js";
import type { IncomingMessage, ServerResponse } from "node:http";

/**
 * Request handler function compatible with Node.js HTTP server.
 *
 * Plugins provide a `handler` that receives standard Node.js HTTP
 * request/response objects. This works with any framework:
 *
 * - Plain `http.createServer` callback
 * - Express `app` (Express apps are valid request handlers)
 * - Fastify with `fastify.server` after `await fastify.ready()`
 * - Koa with `app.callback()`
 * - Hono with `serve()` adapter
 */
export type RequestHandler = (
  req: IncomingMessage,
  res: ServerResponse
) => void | Promise<void>;

export interface TempsPlugin {
  /** Plugin manifest describing capabilities, nav entries, and event subscriptions. */
  manifest(): PluginManifest;

  /**
   * Return a Node.js HTTP request handler for your plugin's routes.
   *
   * The handler receives requests already stripped of the `/x/{plugin_name}/` prefix.
   * Auth context is available via `x-temps-*` headers on each request.
   *
   * @param ctx - Plugin context with access to platform client and data directory.
   */
  handler(ctx: PluginContext): RequestHandler | Promise<RequestHandler>;

  /**
   * Optional: path to a directory containing compiled UI assets to serve
   * under `/ui/`. If provided, the SDK will automatically serve these
   * with proper caching headers and SPA fallback.
   *
   * For development mode with hot-reload.
   */
  uiDistPath?(): string;

  /**
   * Optional: in-memory map of embedded UI assets (for compiled binaries).
   *
   * Use the `embed-assets.ts` script to generate this map at build time.
   * This is the Node.js equivalent of Rust's `include_dir!` macro.
   *
   * Takes priority over `uiDistPath()` if both are provided.
   */
  embeddedUiAssets?(): EmbeddedAssets;

  /**
   * Called after the platform channel is established. Use for initialization
   * logic that needs access to platform data.
   */
  onStart?(ctx: PluginContext): void | Promise<void>;

  /** Called on graceful shutdown (SIGTERM). */
  onShutdown?(): void | Promise<void>;

  /**
   * Called when a subscribed platform event is received.
   */
  onEvent?(ctx: PluginContext, event: PluginEvent): void | Promise<void>;
}
