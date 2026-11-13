// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { client } from '@/api/client/client.gen'

type ClientConfig = ReturnType<typeof client.getConfig>

/**
 * Puts the shared API client back to a config saved with `client.getConfig()`.
 *
 * `client.setConfig(saved)` is not enough: it merges, so a `fetch` or
 * `baseUrl` a test installed survives when the saved config had none, and
 * every later test file in the same `bun test` process that mocks
 * `globalThis.fetch` talks to that test's fake server instead.
 */
export function restoreApiClient(saved: ClientConfig) {
  client.setConfig({ ...saved, baseUrl: saved.baseUrl, fetch: saved.fetch })
}
