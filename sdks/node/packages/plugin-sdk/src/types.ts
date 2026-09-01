// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Canonical TypeScript representation of Temps external-plugin protocol v2. */

import type { PluginContext } from "./context.js";
import type { EmbeddedAssets } from "./ui.js";
import type { IncomingMessage, ServerResponse } from "node:http";

export const EXTERNAL_PLUGIN_PROTOCOL_VERSION = 2 as const;
export const MAX_CALL_BODY_BYTES = 32 * 1024 * 1024;
export const MAX_PLUGIN_REQUEST_BODY_BYTES = 32 * 1024 * 1024;
export const MAX_PLUGIN_RESPONSE_BODY_BYTES = 32 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Manifest and startup handshake
// ---------------------------------------------------------------------------

export type NavSection = "platform" | "settings" | "project";
export type PluginCapability = "api_read" | "api_write";

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
  requires_host_data_access: boolean;
  health_path: string;
  hide_header: boolean;
  public_paths: string[];
  capabilities: PluginCapability[];
  events: string[];
}

export interface PluginHelloMessage {
  type: "hello";
  protocol_version: typeof EXTERNAL_PLUGIN_PROTOCOL_VERSION;
  manifest: PluginManifest;
}

export interface PluginReadyMessage {
  type: "ready";
  ready: true;
  has_ui: boolean;
  protocol_version: typeof EXTERNAL_PLUGIN_PROTOCOL_VERSION;
  openapi?: Record<string, unknown>;
}

export type HandshakeMessage = PluginHelloMessage | PluginReadyMessage;

export interface PluginLaunchConfig {
  protocol_version: number;
  auth_secret: string;
  database_url: string | null;
  host_data_dir: string | null;
}

export interface PluginArgs {
  socketPath: string;
  dataDir: string;
  hostApiUrl?: string;
}

// ---------------------------------------------------------------------------
// Authenticated proxy context
// ---------------------------------------------------------------------------

export type PluginRole =
  | "admin"
  | "platform_admin"
  | "user"
  | "reader"
  | "api_reader"
  | "custom"
  | "metrics_ingest";

export interface TempsUserContext {
  readonly userId: number;
  readonly userEmail: string;
  readonly role: PluginRole;
  readonly permissions: ReadonlySet<string>;
  readonly requestId: string;
  isAdmin(): boolean;
  hasPermission(permission: string): boolean;
}

// ---------------------------------------------------------------------------
// Channel protocol
// ---------------------------------------------------------------------------

export type ChannelErrorCode =
  | "method_not_found"
  | "invalid_params"
  | "permission_denied"
  | "unauthenticated"
  | "not_found"
  | "internal";

export interface ChannelError {
  code: ChannelErrorCode;
  message: string;
}

export interface PluginEvent {
  id: string;
  event_type: string;
  timestamp: string;
  project_id?: number;
  data: Record<string, unknown>;
}

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

export type HttpMethod = "GET" | "POST" | "PUT" | "PATCH" | "DELETE";

export type PartContent =
  | { kind: "text"; value: string }
  | { kind: "binary"; value: string };

export interface MultipartPart {
  name: string;
  filename?: string;
  content_type?: string;
  content: PartContent;
}

export type CallBody =
  | { kind: "json"; body: string }
  | { kind: "multipart"; body: MultipartPart[] };

export interface ApiCall {
  method: HttpMethod;
  path: string;
  query?: string;
  body?: CallBody;
  actor: string;
}

export interface ApiCallResult {
  status: number;
  /** JSON text carried by the protocol's transparent JsonBody wrapper. */
  body: string;
}

export interface PlatformCallMap {
  get_project: {
    params: { project_id: number };
    result: ProjectInfo;
  };
  list_projects: {
    params: Record<string, never>;
    result: ProjectInfo[];
  };
  get_environment: {
    params: { environment_id: number };
    result: EnvironmentInfo;
  };
  list_environments: {
    params: { project_id: number };
    result: EnvironmentInfo[];
  };
  get_deployment: {
    params: { deployment_id: number };
    result: DeploymentInfo;
  };
  get_last_deployment: {
    params: { project_id: number; environment_id?: number };
    result: DeploymentInfo;
  };
  list_deployments: {
    params: { project_id: number; environment_id?: number; limit?: number };
    result: DeploymentInfo[];
  };
  api_call: {
    params: ApiCall;
    result: ApiCallResult;
  };
}

export type PlatformMethod = keyof PlatformCallMap;
export type PlatformCallRequest = {
  [Method in PlatformMethod]: {
    method: Method;
    params: PlatformCallMap[Method]["params"];
  };
}[PlatformMethod];

export type PlatformCallResponse = {
  [Method in PlatformMethod]: {
    method: Method;
    result: PlatformCallMap[Method]["result"];
  };
}[PlatformMethod];

export interface ChannelRequest {
  type: "request";
  id: number;
  call: PlatformCallRequest;
}

export interface ChannelResponse {
  type: "response";
  id: number;
  outcome: { ok: PlatformCallResponse } | { err: ChannelError };
}

export interface ChannelEvent {
  type: "event";
  event: PluginEvent;
}

export type ChannelMessage = ChannelRequest | ChannelResponse | ChannelEvent;

// ---------------------------------------------------------------------------
// User-facing plugin definition
// ---------------------------------------------------------------------------

export type RequestHandler = (
  req: IncomingMessage,
  res: ServerResponse
) => void | Promise<void>;

export interface TempsPlugin {
  manifest(): PluginManifest;
  handler(ctx: PluginContext): RequestHandler | Promise<RequestHandler>;
  uiDistPath?(): string;
  embeddedUiAssets?(): EmbeddedAssets;
  openapiSchema?(): Record<string, unknown>;
  onStart?(ctx: PluginContext): void | Promise<void>;
  onShutdown?(): void | Promise<void>;
  onEvent?(ctx: PluginContext, event: PluginEvent): void | Promise<void>;
}
