// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { GlobalRegistrator } from '@happy-dom/global-registrator'

/**
 * Preloaded for every `bun test` run (see `bunfig.toml`).
 *
 * Interactive component tests (typing, clicking) need a browser-like DOM, and
 * it has to exist *before* any UI module is evaluated: Radix decides at import
 * time whether layout effects run, so a DOM registered later leaves portals
 * (dropdowns, dialogs) permanently empty.
 *
 * happy-dom replaces a number of runtime primitives that Bun already
 * implements natively and that other tests in this repo rely on (ZIP/Blob
 * handling, fetch, URL, streams). Only the DOM surface is wanted, so those
 * are put back to Bun's own implementations right after registering.
 */
const NATIVE_GLOBALS = [
  'fetch',
  'Request',
  'Response',
  'Headers',
  'Blob',
  'File',
  'FormData',
  'URL',
  'URLSearchParams',
  'AbortController',
  'AbortSignal',
  'TextEncoder',
  'TextDecoder',
  'ReadableStream',
  'WritableStream',
  'TransformStream',
  'WebSocket',
  'EventSource',
  'setTimeout',
  'clearTimeout',
  'setInterval',
  'clearInterval',
  'queueMicrotask',
  'structuredClone',
  'performance',
  'crypto',
] as const

if (!GlobalRegistrator.isRegistered) {
  const target = globalThis as unknown as Record<string, unknown>
  const natives = NATIVE_GLOBALS.map((name) => [name, target[name]] as const)
  GlobalRegistrator.register({ url: 'http://localhost/' })
  for (const [name, value] of natives) {
    if (value !== undefined) target[name] = value
  }
}
;(
  globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
).IS_REACT_ACT_ENVIRONMENT = true
