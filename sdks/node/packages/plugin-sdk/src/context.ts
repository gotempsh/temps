// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * PluginContext -- provides access to platform client and plugin metadata.
 *
 * Mirrors: crates/temps-plugin-sdk/src/context.rs
 */

import type { TempsClient } from "./client.js";

export class PluginContext {
  private readonly _pluginName: string;
  private readonly _dataDir: string;
  private readonly _authSecret: string;
  private readonly _client: TempsClient;

  constructor(options: {
    pluginName: string;
    dataDir: string;
    authSecret: string;
    client: TempsClient;
  }) {
    this._pluginName = options.pluginName;
    this._dataDir = options.dataDir;
    this._authSecret = options.authSecret;
    this._client = options.client;
  }

  /** The platform data client for querying projects, deployments, etc. */
  get temps(): TempsClient {
    return this._client;
  }

  /** The plugin's unique name (kebab-case). */
  get pluginName(): string {
    return this._pluginName;
  }

  /**
   * Persistent data directory for this plugin.
   * Use this for SQLite databases, config files, caches, etc.
   */
  get dataDir(): string {
    return this._dataDir;
  }

  /** HMAC secret for validating request authenticity. */
  get authSecret(): string {
    return this._authSecret;
  }
}
