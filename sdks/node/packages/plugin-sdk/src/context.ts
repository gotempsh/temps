// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { PlatformApi } from "./api.js";
import type { AuthenticatedCaller } from "./auth.js";
import type { TempsClient } from "./client.js";

export class PluginContext {
  readonly #pluginName: string;
  readonly #dataDir: string;
  readonly #databaseUrl?: string;
  readonly #hostDataDir?: string;
  readonly #hostApiUrl?: string;
  readonly #authSecret: string;
  readonly #client: TempsClient;

  constructor(options: {
    pluginName: string;
    dataDir: string;
    databaseUrl?: string;
    hostDataDir?: string;
    hostApiUrl?: string;
    authSecret: string;
    client: TempsClient;
  }) {
    this.#pluginName = options.pluginName;
    this.#dataDir = options.dataDir;
    this.#databaseUrl = options.databaseUrl;
    this.#hostDataDir = options.hostDataDir;
    this.#hostApiUrl = options.hostApiUrl;
    this.#authSecret = options.authSecret;
    this.#client = options.client;
  }

  get temps(): TempsClient {
    return this.#client;
  }

  apiAsCaller(caller: AuthenticatedCaller): PlatformApi {
    return new PlatformApi(this.#client, caller);
  }

  get pluginName(): string {
    return this.#pluginName;
  }

  get dataDir(): string {
    return this.#dataDir;
  }

  get databaseUrl(): string | undefined {
    return this.#databaseUrl;
  }

  get hostDataDir(): string | undefined {
    return this.#hostDataDir;
  }

  get hostApiUrl(): string | undefined {
    return this.#hostApiUrl;
  }

  get mountUrl(): string | undefined {
    return this.#hostApiUrl === undefined
      ? undefined
      : `${this.#hostApiUrl.replace(/\/$/, "")}/api/x/${this.#pluginName}`;
  }

  /** Per-process assertion secret. Prefer extractAuthContext over manual use. */
  get authSecret(): string {
    return this.#authSecret;
  }
}
