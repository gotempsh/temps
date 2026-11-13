// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach } from 'bun:test'
import { client } from '@/api/client/client.gen'
import { restoreAllMockApis } from './mock-registry'

// The client's transport before any test changes it. `setConfig` merges, so
// a test that restores the config it read with `getConfig()` keeps its own
// fake `fetch` whenever the original had none, and every later test file
// that mocks `globalThis.fetch` then reaches that fake instead.
const pristine = client.getConfig()

// Preloaded hooks apply to every test file: a failing assertion must not leave
// a test's fake API routes, fetch or base URL installed on the shared client,
// and nothing a test stored (the dock's open chat, a dismissed banner) may
// decide what a later test file renders.
afterEach(() => {
  restoreAllMockApis()
  client.setConfig({ baseUrl: pristine.baseUrl, fetch: pristine.fetch })
  globalThis.localStorage?.clear()
  globalThis.sessionStorage?.clear()
})
