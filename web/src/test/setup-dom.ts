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

/**
 * `expect` pretty-prints a failing value by walking its object graph, and a
 * happy-dom node reaches the shared window that every earlier test file has
 * added to. A few hundred files into a run, one failed `expect(element)`
 * blocks the event loop for seconds -- and `waitFor` retries a failing
 * assertion by design, so `waitFor(() => expect(el).toBe(other))` outlived the
 * 5s test timeout on CI, where files run in a different order than on macOS.
 * Print nodes as their (truncated) markup instead.
 */
const INSPECT = Symbol.for('nodejs.util.inspect.custom')
const MAX_INSPECTED_MARKUP = 300
const NodeConstructor = (globalThis as { Node?: { prototype: object } }).Node
if (
  NodeConstructor &&
  !Object.prototype.hasOwnProperty.call(NodeConstructor.prototype, INSPECT)
) {
  Object.defineProperty(NodeConstructor.prototype, INSPECT, {
    configurable: true,
    value(this: Node) {
      const markup =
        this instanceof Element
          ? this.outerHTML
          : `${this.nodeName} ${JSON.stringify(this.textContent ?? '')}`
      return markup.length > MAX_INSPECTED_MARKUP
        ? `${markup.slice(0, MAX_INSPECTED_MARKUP)}…`
        : markup
    },
  })
}
