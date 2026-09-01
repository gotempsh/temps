// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Typed error hierarchy for the plugin SDK.
 */

import type { ChannelErrorCode } from "./types.js";

export class PluginSdkError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "PluginSdkError";
  }
}

export class ArgsError extends PluginSdkError {
  constructor(message: string) {
    super(`Argument error: ${message}`);
    this.name = "ArgsError";
  }
}

export class SocketBindError extends PluginSdkError {
  public readonly path: string;
  public readonly reason: string;

  constructor(path: string, reason: string) {
    super(`Failed to bind Unix socket at ${path}: ${reason}`);
    this.name = "SocketBindError";
    this.path = path;
    this.reason = reason;
  }
}

export class HandshakeError extends PluginSdkError {
  public readonly pluginName: string;
  public readonly reason: string;

  constructor(pluginName: string, reason: string) {
    super(`Handshake failed for plugin "${pluginName}": ${reason}`);
    this.name = "HandshakeError";
    this.pluginName = pluginName;
    this.reason = reason;
  }
}

export class InitializationError extends PluginSdkError {
  public readonly pluginName: string;
  public readonly reason: string;

  constructor(pluginName: string, reason: string) {
    super(`Initialization failed for plugin "${pluginName}": ${reason}`);
    this.name = "InitializationError";
    this.pluginName = pluginName;
    this.reason = reason;
  }
}

export class ChannelClosedError extends PluginSdkError {
  constructor() {
    super("Platform channel is closed");
    this.name = "ChannelClosedError";
  }
}

export class PlatformError extends PluginSdkError {
  public readonly code: ChannelErrorCode;

  constructor(code: ChannelErrorCode, message: string) {
    super(`Platform error (${code}): ${message}`);
    this.name = "PlatformError";
    this.code = code;
  }
}

export class ChannelTimeoutError extends PluginSdkError {
  public readonly method: string;
  public readonly timeoutMs: number;

  constructor(method: string, timeoutMs: number) {
    super(`Channel request "${method}" timed out after ${timeoutMs}ms`);
    this.name = "ChannelTimeoutError";
    this.method = method;
    this.timeoutMs = timeoutMs;
  }
}
