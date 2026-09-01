// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export { runPlugin, parsePluginArgs } from "./runtime.js";
export { ManifestBuilder, createManifest } from "./manifest-builder.js";
export { PluginContext } from "./context.js";
export { TempsClient } from "./client.js";
export { PlatformApi } from "./api.js";
export type {
  PlatformApiRequestOptions,
  PlatformApiResponse,
} from "./api.js";
export {
  AuthenticatedCaller,
  extractAuthContext,
  secureSecretMatches,
  verifyProxyHeaders,
} from "./auth.js";
export {
  encodeHandshakeMessage,
  emitHello,
  emitReady,
  parseLaunchConfigLine,
  readLaunchConfig,
  HEADER_PREFIX,
  HEADER_PLUGIN_NAME,
  HEADER_USER_ID,
  HEADER_USER_EMAIL,
  HEADER_USER_ROLE,
  HEADER_USER_PERMISSIONS,
  HEADER_REQUEST_ID,
  HEADER_AUTH_SIGNATURE,
  HEADER_ACTOR_TOKEN,
  PLUGIN_CHANNEL_PATH,
  PLUGIN_EVENTS_PATH,
} from "./protocol.js";
export { createUiHandler, createEmbeddedUiHandler } from "./ui.js";
export type { EmbeddedFile, EmbeddedAssets } from "./ui.js";
export {
  PluginSdkError,
  ArgsError,
  SocketBindError,
  HandshakeError,
  InitializationError,
  ChannelClosedError,
  PlatformError,
  ChannelTimeoutError,
  ProtocolMismatchError,
  ActorTokenRequiredError,
  CallBodyTooLargeError,
  RequestBodyTooLargeError,
  ResponseBodyTooLargeError,
} from "./errors.js";

export {
  EXTERNAL_PLUGIN_PROTOCOL_VERSION,
  MAX_CALL_BODY_BYTES,
  MAX_PLUGIN_REQUEST_BODY_BYTES,
  MAX_PLUGIN_RESPONSE_BODY_BYTES,
} from "./types.js";
export type {
  TempsPlugin,
  RequestHandler,
  PluginManifest,
  PluginCapability,
  NavEntry,
  NavSection,
  UiManifest,
  UiRoute,
  HandshakeMessage,
  PluginHelloMessage,
  PluginReadyMessage,
  PluginLaunchConfig,
  ChannelMessage,
  ChannelRequest,
  ChannelResponse,
  ChannelEvent,
  ChannelError,
  ChannelErrorCode,
  PlatformCallMap,
  PlatformCallRequest,
  PlatformCallResponse,
  PlatformMethod,
  PluginEvent,
  ProjectInfo,
  EnvironmentInfo,
  DeploymentInfo,
  PluginRole,
  TempsUserContext,
  HttpMethod,
  PartContent,
  MultipartPart,
  CallBody,
  ApiCall,
  ApiCallResult,
  PluginArgs,
} from "./types.js";
