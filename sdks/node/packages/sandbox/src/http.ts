// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { SandboxError } from './errors.js';

// Thin fetch wrapper. Centralises auth header, base URL resolution, and
// the snake_case → camelCase conversion so the rest of the SDK can stay
// focused on semantics. Do not export — callers use the `Sandbox` class.

export interface ResolvedConfig {
  apiUrl: string;
  apiToken: string;
  fetch: typeof fetch;
}

export function resolveConfig(
  cfg: { apiUrl?: string; apiToken?: string; fetch?: typeof fetch } = {}
): ResolvedConfig {
  const env =
    typeof process !== 'undefined' && process.env ? process.env : {};
  const apiUrl = cfg.apiUrl ?? env.TEMPS_API_URL;
  const apiToken = cfg.apiToken ?? env.TEMPS_API_TOKEN;
  if (!apiUrl) throw SandboxError.missingConfig('apiUrl');
  if (!apiToken) throw SandboxError.missingConfig('apiToken');
  const fetchImpl = cfg.fetch ?? globalThis.fetch;
  if (!fetchImpl) {
    throw new SandboxError(
      'No fetch implementation available. Pass `fetch` in config or run on Node 18+.',
      { code: 'NO_FETCH' }
    );
  }
  return { apiUrl: apiUrl.replace(/\/$/, ''), apiToken, fetch: fetchImpl };
}

export async function request<T>(
  cfg: ResolvedConfig,
  method: string,
  path: string,
  body?: unknown,
  query?: Record<string, string | number | undefined>
): Promise<T> {
  const url = new URL(cfg.apiUrl + path);
  if (query) {
    for (const [k, v] of Object.entries(query)) {
      if (v !== undefined) url.searchParams.set(k, String(v));
    }
  }
  const headers: Record<string, string> = {
    Authorization: `Bearer ${cfg.apiToken}`,
    Accept: 'application/json',
  };
  if (body !== undefined) headers['Content-Type'] = 'application/json';

  let response: Response;
  try {
    response = await cfg.fetch(url.toString(), {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
    });
  } catch (e) {
    throw SandboxError.networkError(e as Error);
  }

  if (!response.ok) {
    // RFC 7807 uses application/problem+json; be lenient and try to
    // parse whatever we got before giving up on a generic message.
    let parsed: unknown;
    try {
      parsed = await response.json();
    } catch {
      parsed = undefined;
    }
    throw SandboxError.fromResponse(response, parsed as never);
  }

  if (response.status === 204) return undefined as T;
  return (await response.json()) as T;
}
